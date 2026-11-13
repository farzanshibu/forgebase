//! Notifiers (ADR-0035 §4): signed webhook, Slack, email, PagerDuty
//! Events v2 and a queue topic. HTTP goes through the SSRF-safe fetcher.

use std::collections::BTreeMap;

use forgebase_data::AccessContext;
use forgebase_functions::fetch::FetchRequest;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::model::NotifierKind;
use crate::service::{AlertService, NotifierRow};

/// Email template key the email notifier renders (created when missing;
/// edit it like any other template).
pub const EMAIL_TEMPLATE: &str = "alert_notification";

/// Outcome of one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub(crate) ok: bool,
    pub(crate) status: Option<i64>,
    pub(crate) error: Option<String>,
}

impl Attempt {
    pub(crate) fn sent(status: Option<i64>) -> Self {
        Self {
            ok: true,
            status,
            error: None,
        }
    }

    pub(crate) fn failed(status: Option<i64>, error: impl Into<String>) -> Self {
        let mut e: String = error.into();
        if e.len() > 500 {
            let mut end = 500;
            while !e.is_char_boundary(end) {
                end -= 1;
            }
            e.truncate(end);
        }
        Self {
            ok: false,
            status,
            error: Some(e),
        }
    }
}

/// A short title: `[FIRING:2] HighErrorRate`.
fn title(payload: &Json) -> String {
    let status = payload["status"].as_str().unwrap_or("firing");
    let alerts = payload["alerts"].as_array().map_or(&[][..], Vec::as_slice);
    let firing = alerts.iter().filter(|a| a["status"] == "firing").count();
    let name = payload["group_labels"]["alertname"]
        .as_str()
        .or_else(|| alerts.first().and_then(|a| a["name"].as_str()))
        .unwrap_or("alert");
    match status {
        "resolved" => format!("[RESOLVED] {name}"),
        "test" => format!("[TEST] {name}"),
        _ => format!("[FIRING:{firing}] {name}"),
    }
}

/// One line per alert: severity, summary (or name and labels).
fn lines(payload: &Json) -> Vec<String> {
    payload["alerts"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    let summary = x["annotations"]["summary"].as_str().map(str::to_owned).unwrap_or_else(|| {
                        let labels: BTreeMap<String, String> =
                            serde_json::from_value(x["labels"].clone()).unwrap_or_default();
                        labels
                            .iter()
                            .filter(|(k, _)| k.as_str() != "alertname" && k.as_str() != "severity")
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    });
                    format!(
                        "{} {} ({}): {summary}",
                        if x["status"] == "resolved" { "RESOLVED" } else { "FIRING" },
                        x["name"].as_str().unwrap_or(""),
                        x["severity"].as_str().unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pd_severity(payload: &Json) -> &'static str {
    let alerts = payload["alerts"].as_array().map_or(&[][..], Vec::as_slice);
    if alerts.iter().any(|a| a["severity"] == "critical") {
        "critical"
    } else if alerts.iter().any(|a| a["severity"] == "warning") {
        "warning"
    } else {
        "info"
    }
}

/// PagerDuty dedup key of a notifier's group.
pub(crate) fn dedup_key(notifier_id: &str, group_key: &str) -> String {
    let mut h = Sha256::new();
    h.update(notifier_id.as_bytes());
    h.update([0u8]);
    h.update(group_key.as_bytes());
    format!("forgebase-{}", forgebase_observability::trace::hex(&h.finalize()[..16]))
}

impl AlertService {
    async fn post(&self, url: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> Attempt {
        let Some(fetcher) = &self.inner.deps.fetcher else {
            return Attempt::failed(None, "no outbound HTTP client is configured on this server");
        };
        let req = FetchRequest {
            method: "POST".into(),
            url: url.to_owned(),
            headers,
            body,
        };
        match fetcher.fetch(req, &[]).await {
            Ok(r) if (200..300).contains(&r.status) => Attempt::sent(Some(i64::from(r.status))),
            Ok(r) => {
                let snippet = String::from_utf8_lossy(&r.body[..r.body.len().min(200)]).into_owned();
                Attempt::failed(Some(i64::from(r.status)), format!("HTTP {}: {snippet}", r.status))
            }
            Err(e) => Attempt::failed(None, e.to_string()),
        }
    }

    /// Delivers `payload` (`event`: `firing`, `resolved` or `test`) to `n`.
    pub(crate) async fn send(&self, n: &NotifierRow, delivery_id: &str, event: &str, payload: &Json) -> Attempt {
        let secret = match self.open_secret(n) {
            Ok(s) => s,
            Err(e) => return Attempt::failed(None, e.to_string()),
        };
        match n.kind {
            NotifierKind::Webhook => {
                let (Some(url), Some(secret)) = (&n.url, secret) else {
                    return Attempt::failed(None, "the webhook notifier has no URL or signing secret");
                };
                let body = serde_json::to_vec(payload).unwrap_or_default();
                let ts = self.now().div_euclid(1000);
                let headers = vec![
                    ("content-type".to_owned(), "application/json".to_owned()),
                    (
                        forgebase_queue::push::SIGNATURE_HEADER.to_owned(),
                        forgebase_queue::push::sign(&secret, ts, &body),
                    ),
                    ("x-forgebase-event".to_owned(), format!("alert.{event}")),
                    ("x-forgebase-delivery".to_owned(), delivery_id.to_owned()),
                ];
                self.post(url, headers, body).await
            }
            NotifierKind::Slack => {
                let Some(url) = secret else {
                    return Attempt::failed(None, "the Slack notifier has no webhook URL");
                };
                let mut text = format!("*{}*", title(payload));
                for l in lines(payload) {
                    text.push_str("\n• ");
                    text.push_str(&l);
                }
                if let Some(u) = payload["external_url"].as_str() {
                    text.push_str(&format!("\n<{u}|Open ForgeBase>"));
                }
                let body = serde_json::to_vec(&json!({"text": text})).unwrap_or_default();
                self.post(&url, vec![("content-type".into(), "application/json".into())], body)
                    .await
            }
            NotifierKind::Pagerduty => {
                let (Some(url), Some(key)) = (&n.url, secret) else {
                    return Attempt::failed(None, "the PagerDuty notifier has no URL or routing key");
                };
                let group = payload["group_key"].as_str().unwrap_or("");
                let dedup = dedup_key(&n.id, group);
                let headers = vec![("content-type".to_owned(), "application/json".to_owned())];
                let trigger = json!({
                    "routing_key": key,
                    "event_action": "trigger",
                    "dedup_key": dedup,
                    "client": "ForgeBase",
                    "client_url": payload["external_url"],
                    "payload": {
                        "summary": format!("{}: {}", title(payload), lines(payload).join("; ")).chars().take(1000).collect::<String>(),
                        "source": "forgebase",
                        "severity": pd_severity(payload),
                        "custom_details": {"group_labels": payload["group_labels"], "alerts": payload["alerts"]},
                    },
                });
                let resolve = json!({"routing_key": key, "event_action": "resolve", "dedup_key": dedup});
                match event {
                    "resolved" => {
                        self.post(url, headers, serde_json::to_vec(&resolve).unwrap_or_default())
                            .await
                    }
                    "test" => {
                        // A test opens and closes an incident at once.
                        let a = self
                            .post(url, headers.clone(), serde_json::to_vec(&trigger).unwrap_or_default())
                            .await;
                        if !a.ok {
                            return a;
                        }
                        self.post(url, headers, serde_json::to_vec(&resolve).unwrap_or_default())
                            .await
                    }
                    _ => {
                        self.post(url, headers, serde_json::to_vec(&trigger).unwrap_or_default())
                            .await
                    }
                }
            }
            NotifierKind::Email => self.send_email(n, payload).await,
            NotifierKind::Queue => {
                let Some(q) = &self.inner.deps.queue else {
                    return Attempt::failed(None, "queues are switched off on this server");
                };
                let Some(topic) = &n.topic else {
                    return Attempt::failed(None, "the queue notifier has no topic");
                };
                let msg = forgebase_queue::NewMessage {
                    data: payload.clone(),
                    attributes: [
                        ("status".to_owned(), payload["status"].as_str().unwrap_or("").to_owned()),
                        (
                            "alertname".to_owned(),
                            payload["group_labels"]["alertname"].as_str().unwrap_or("").to_owned(),
                        ),
                        ("severity".to_owned(), pd_severity(payload).to_owned()),
                    ]
                    .into_iter()
                    .collect(),
                    ordering_key: None,
                    // A retry after a lost answer is not published twice.
                    dedup_id: Some(format!("alert:{delivery_id}")),
                };
                match q.publish(&AccessContext::system(), topic, vec![msg]).await {
                    Ok(_) => Attempt::sent(None),
                    Err(e) => Attempt::failed(None, e.to_string()),
                }
            }
        }
    }

    async fn send_email(&self, n: &NotifierRow, payload: &Json) -> Attempt {
        let Some(c) = &self.inner.deps.customize else {
            return Attempt::failed(None, "email is not available on this server");
        };
        let sys = AccessContext::system();
        if !self.inner.template_ready.load(std::sync::atomic::Ordering::Acquire) {
            if c.template(&sys, None, EMAIL_TEMPLATE).await.is_err() {
                let t = forgebase_customize::TemplateInput {
                    subject: "{{ subject }}".into(),
                    text: "{{ text }}".into(),
                    html: None,
                };
                if let Err(e) = c.put_template(&sys, None, EMAIL_TEMPLATE, t).await {
                    return Attempt::failed(None, format!("the {EMAIL_TEMPLATE} email template: {e}"));
                }
            }
            self.inner
                .template_ready
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let subject = title(payload);
        let mut text = String::new();
        for l in lines(payload) {
            text.push_str(&l);
            text.push('\n');
        }
        if let Some(u) = payload["external_url"].as_str() {
            text.push_str(&format!("\n{u}\n"));
        }
        for to in &n.to {
            let input = forgebase_customize::SendInput {
                template: EMAIL_TEMPLATE.into(),
                to: to.clone(),
                variables: json!({"subject": subject, "text": text, "status": payload["status"]}),
            };
            if let Err(e) = c.send(&sys, None, input).await {
                return Attempt::failed(None, format!("email to {to}: {e}"));
            }
        }
        Attempt::sent(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_lines() {
        let p = json!({
            "status": "firing",
            "group_labels": {"alertname": "QueuePushPaused"},
            "alerts": [
                {"status": "firing", "name": "QueuePushPaused", "severity": "critical",
                 "annotations": {"summary": "Push delivery of orders is paused"}, "labels": {}},
                {"status": "resolved", "name": "QueuePushPaused", "severity": "critical",
                 "annotations": {}, "labels": {"subscription": "b", "alertname": "QueuePushPaused"}},
            ],
        });
        assert_eq!(title(&p), "[FIRING:1] QueuePushPaused");
        let l = lines(&p);
        assert_eq!(l[0], "FIRING QueuePushPaused (critical): Push delivery of orders is paused");
        assert_eq!(l[1], "RESOLVED QueuePushPaused (critical): subscription=b");
        assert_eq!(pd_severity(&p), "critical");
        assert_eq!(dedup_key("n", "g"), dedup_key("n", "g"));
        assert_ne!(dedup_key("n", "g"), dedup_key("n", "h"));
    }
}
