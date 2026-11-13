//! Transactional email: templates per realm, rendering and preview, sending
//! through the job queue with retries, and the delivery log.
//!
//! `send` renders the template, writes a `queued` row to the delivery log and
//! enqueues a `customize.email.send` job (ForgeBase's queue, ADR-0007, retries
//! with exponential backoff up to `email_max_attempts`). The job calls
//! [`CustomizeService::deliver`], which hands the message to the mail
//! provider and records the result: `sent`, `retrying` (the queue will call
//! again) or `failed` (a permanent refusal or the last attempt). The rendered
//! body is kept only while a message may still be (re)sent: it is cleared
//! once sent, and from failed messages after `failed_body_retention_ms`.

use forgebase_data::AccessContext;
use forgebase_db::Value;
use forgebase_jobs::NewJob;
use serde_json::{Value as Json, json};

use crate::error::{CustomizeError, Result};
use crate::mail::{OutMail, SendError, from_header, valid_address};
use crate::model::{Attempt, Delivery, DeliveryStatus, EmailTemplate, Preview, PreviewInput, SendInput, TemplateInput};
use crate::render::{self, MAX_HTML, MAX_SUBJECT, MAX_TEXT, Mode};
use crate::service::{CustomizeService, EMAIL_JOB, check_key, int, new_id, opt_int, opt_text, text};

/// Largest variable payload of one message.
const MAX_VARIABLES: usize = 16 * 1024;

/// The built-in templates, used by a realm that has not saved its own.
pub fn builtin(key: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match key {
        "verify_email" => (
            "Confirm your email for {{ app_name }}",
            "Welcome to {{ app_name }}.\n\nConfirm your email address: {{ link }}\n\nIf you did not sign up, ignore this message.",
            "<p>Welcome to {{ app_name }}.</p><p><a href=\"{{ link }}\">Confirm your email address</a></p><p>If you did not sign up, ignore this message.</p>",
        ),
        "password_reset" => (
            "Reset your {{ app_name }} password",
            "Reset your password: {{ link }}\n\nIf you did not ask for this, ignore this message.",
            "<p><a href=\"{{ link }}\">Reset your password</a></p><p>If you did not ask for this, ignore this message.</p>",
        ),
        "magic_link" => (
            "Your {{ app_name }} sign-in link",
            "Sign in to {{ app_name }}: {{ link }}",
            "<p><a href=\"{{ link }}\">Sign in to {{ app_name }}</a></p>",
        ),
        "email_otp" => (
            "Your {{ app_name }} code: {{ code }}",
            "Your {{ app_name }} sign-in code is {{ code }}.",
            "<p>Your {{ app_name }} sign-in code is <b>{{ code }}</b>.</p>",
        ),
        "invitation" => (
            "You are invited to {{ app_name }}",
            "{{ inviter }} invited you to {{ app_name }}: {{ link }}",
            "<p>{{ inviter }} invited you to {{ app_name }}.</p><p><a href=\"{{ link }}\">Accept the invitation</a></p>",
        ),
        _ => return None,
    })
}

/// Keys of the built-in templates.
pub const BUILTIN_KEYS: [&str; 5] = [
    "verify_email",
    "password_reset",
    "magic_link",
    "email_otp",
    "invitation",
];

fn template_of(
    realm: &str,
    key: &str,
    subject: &str,
    text_t: &str,
    html: Option<&str>,
    customized: bool,
    at: Option<i64>,
) -> Result<EmailTemplate> {
    let mut vars = render::variables(subject)?;
    vars.extend(render::variables(text_t)?);
    if let Some(h) = html {
        vars.extend(render::variables(h)?);
    }
    vars.sort();
    vars.dedup();
    Ok(EmailTemplate {
        realm: realm.to_owned(),
        key: key.to_owned(),
        subject: subject.to_owned(),
        text: text_t.to_owned(),
        html: html.map(str::to_owned),
        customized,
        variables: vars,
        updated_at: at,
    })
}

fn validate_input(i: &TemplateInput) -> Result<()> {
    let too_long = |what: &str, max: usize| {
        CustomizeError::invalid("invalid_template", format!("{what} is longer than {max} bytes"))
    };
    if i.subject.trim().is_empty() {
        return Err(CustomizeError::invalid("invalid_template", "a subject is required"));
    }
    if i.text.trim().is_empty() {
        return Err(CustomizeError::invalid("invalid_template", "a text body is required"));
    }
    if i.subject.len() > MAX_SUBJECT {
        return Err(too_long("subject", MAX_SUBJECT));
    }
    if i.text.len() > MAX_TEXT {
        return Err(too_long("text", MAX_TEXT));
    }
    if i.html.as_ref().is_some_and(|h| h.len() > MAX_HTML) {
        return Err(too_long("html", MAX_HTML));
    }
    render::validate(&i.subject)?;
    render::validate(&i.text)?;
    if let Some(h) = &i.html {
        render::validate(h)?;
    }
    Ok(())
}

const DELIVERY_COLUMNS: &str = "id, realm, template, recipient, subject, status, attempts, last_error, provider, job_id, created_at, updated_at, sent_at, provider_message_id";

fn delivery_row(r: &[Value]) -> Delivery {
    Delivery {
        id: text(r, 0),
        realm: text(r, 1),
        template: opt_text(r, 2),
        to: text(r, 3),
        subject: text(r, 4),
        status: DeliveryStatus::parse(&text(r, 5)),
        attempts: int(r, 6),
        last_error: opt_text(r, 7),
        provider: opt_text(r, 8),
        job_id: opt_text(r, 9),
        created_at: int(r, 10),
        updated_at: int(r, 11),
        sent_at: opt_int(r, 12),
        provider_message_id: opt_text(r, 13),
    }
}

/// Filters of the delivery log.
#[derive(Debug, Clone, Default)]
pub struct DeliveryQuery {
    /// Only this status.
    pub status: Option<DeliveryStatus>,
    /// Only this recipient.
    pub to: Option<String>,
    /// Only this template.
    pub template: Option<String>,
    /// Page size (default 50, max 200).
    pub limit: Option<i64>,
    /// Only rows created before this time (epoch ms), for paging.
    pub before: Option<i64>,
}

impl CustomizeService {
    // -----------------------------------------------------------------------
    // Templates
    // -----------------------------------------------------------------------

    async fn stored_template(&self, realm: &str, key: &str) -> Result<Option<EmailTemplate>> {
        let rows = self
            .query(
                "SELECT subject, text_body, html_body, updated_at FROM _forgebase_email_templates WHERE realm = ?1 AND key = ?2",
                vec![realm.into(), key.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| {
                template_of(
                    realm,
                    key,
                    &text(r, 0),
                    &text(r, 1),
                    opt_text(r, 2).as_deref(),
                    true,
                    opt_int(r, 3),
                )
            })
            .transpose()
    }

    /// The template of `key`: the realm's own, else the built-in one.
    pub(crate) async fn resolve_template(&self, realm: &str, key: &str) -> Result<EmailTemplate> {
        if let Some(t) = self.stored_template(realm, key).await? {
            return Ok(t);
        }
        builtin(key)
            .map(|(s, t, h)| template_of(realm, key, s, t, Some(h), false, None))
            .transpose()?
            .ok_or_else(|| CustomizeError::NotFound(format!("template {key:?}")))
    }

    /// Every template of a realm: its own plus the built-ins it does not override.
    pub async fn templates(&self, ctx: &AccessContext, realm: Option<&str>) -> Result<Vec<EmailTemplate>> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        let rows = self
            .query(
                "SELECT key FROM _forgebase_email_templates WHERE realm = ?1 ORDER BY key",
                vec![realm.clone().into()],
            )
            .await?;
        let mut keys: Vec<String> = rows.rows.iter().map(|r| text(r, 0)).collect();
        for k in BUILTIN_KEYS {
            if !keys.iter().any(|x| x == k) {
                keys.push(k.to_owned());
            }
        }
        keys.sort();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            out.push(self.resolve_template(&realm, &k).await?);
        }
        Ok(out)
    }

    /// One template.
    pub async fn template(&self, ctx: &AccessContext, realm: Option<&str>, key: &str) -> Result<EmailTemplate> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        self.resolve_template(&realm, key).await
    }

    /// Saves a realm's template.
    pub async fn put_template(
        &self,
        ctx: &AccessContext,
        realm: Option<&str>,
        key: &str,
        input: TemplateInput,
    ) -> Result<EmailTemplate> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_write(ctx, &realm)?;
        check_key("template key", key, false)?;
        self.require_realm(&realm).await?;
        validate_input(&input)?;
        let mut tx = self.begin().await?;
        tx.exec(
            "INSERT INTO _forgebase_email_templates (realm, key, subject, text_body, html_body, updated_at, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (realm, key) DO UPDATE SET subject = excluded.subject, text_body = excluded.text_body,
                 html_body = excluded.html_body, updated_at = excluded.updated_at, updated_by = excluded.updated_by",
            vec![
                realm.clone().into(),
                key.into(),
                input.subject.into(),
                input.text.into(),
                input.html.into(),
                self.now().into(),
                ctx.principal_key().into(),
            ],
        )
        .await?;
        tx.commit().await?;
        self.resolve_template(&realm, key).await
    }

    /// Removes a realm's template (a built-in key goes back to the built-in text).
    pub async fn delete_template(&self, ctx: &AccessContext, realm: Option<&str>, key: &str) -> Result<()> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_write(ctx, &realm)?;
        let mut tx = self.begin().await?;
        let n = tx
            .exec(
                "DELETE FROM _forgebase_email_templates WHERE realm = ?1 AND key = ?2",
                vec![realm.into(), key.into()],
            )
            .await?;
        tx.commit().await?;
        if n == 0 {
            return Err(CustomizeError::NotFound(format!("template {key:?}")));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// The variables every message gets; the caller cannot override them.
    async fn system_vars(&self, realm: &str) -> Result<serde_json::Map<String, Json>> {
        let b = self.branding_of(realm).await?;
        let mut m = serde_json::Map::new();
        m.insert("realm".into(), json!(realm));
        m.insert(
            "app_name".into(),
            json!(b.display_name.unwrap_or_else(|| realm.to_owned())),
        );
        if let Some(s) = b.support_email {
            m.insert("support_email".into(), json!(s));
        }
        Ok(m)
    }

    async fn merged_vars(&self, realm: &str, vars: &Json) -> Result<Json> {
        let mut m = match vars {
            Json::Null => serde_json::Map::new(),
            Json::Object(o) => o.clone(),
            _ => {
                return Err(CustomizeError::invalid(
                    "invalid_variables",
                    "variables must be a JSON object",
                ));
            }
        };
        if vars.to_string().len() > MAX_VARIABLES {
            return Err(CustomizeError::invalid(
                "invalid_variables",
                "variables are larger than 16 KiB",
            ));
        }
        for (k, v) in self.system_vars(realm).await? {
            m.insert(k, v);
        }
        Ok(Json::Object(m))
    }

    fn render_template(subject: &str, text_t: &str, html: Option<&str>, vars: &Json) -> Result<Preview> {
        let (subject, mut missing) = render::render(subject, vars, Mode::Subject)?;
        let (text_out, m2) = render::render(text_t, vars, Mode::Text)?;
        missing.extend(m2);
        let html = match html {
            Some(h) => {
                let (out, m3) = render::render(h, vars, Mode::Html)?;
                missing.extend(m3);
                Some(out)
            }
            None => None,
        };
        missing.sort();
        missing.dedup();
        Ok(Preview {
            subject: subject.trim().to_owned(),
            text: text_out,
            html,
            missing,
        })
    }

    /// Renders a stored, built-in or draft template with variables, without sending.
    pub async fn preview(&self, ctx: &AccessContext, realm: Option<&str>, input: PreviewInput) -> Result<Preview> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        let vars = self.merged_vars(&realm, &input.variables).await?;
        match (&input.template, &input.draft) {
            (Some(_), Some(_)) | (None, None) => Err(CustomizeError::invalid(
                "invalid_preview",
                "give either `template` (a key) or `draft`",
            )),
            (Some(key), None) => {
                let t = self.resolve_template(&realm, key).await?;
                Self::render_template(&t.subject, &t.text, t.html.as_deref(), &vars)
            }
            (None, Some(d)) => {
                validate_input(d)?;
                Self::render_template(&d.subject, &d.text, d.html.as_deref(), &vars)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Sending and the delivery log
    // -----------------------------------------------------------------------

    /// Renders `input.template` for `realm`, logs the message and queues its
    /// delivery. Needs `customization:write` (or a privileged caller); other
    /// ForgeBase code sends as the system principal.
    pub async fn send(&self, ctx: &AccessContext, realm: Option<&str>, input: SendInput) -> Result<Delivery> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_write(ctx, &realm)?;
        self.require_realm(&realm).await?;
        let to = input.to.trim().to_owned();
        if !valid_address(&to) {
            return Err(CustomizeError::invalid(
                "invalid_recipient",
                "`to` is not a valid email address",
            ));
        }
        let t = self.resolve_template(&realm, &input.template).await?;
        let vars = self.merged_vars(&realm, &input.variables).await?;
        let p = Self::render_template(&t.subject, &t.text, t.html.as_deref(), &vars)?;
        let id = new_id("eml");
        let now = self.now();
        let mut tx = self.begin().await?;
        // Failed messages keep their body only for a while (it may hold a reset link).
        tx.exec(
            "UPDATE _forgebase_email_deliveries SET text_body = NULL, html_body = NULL
             WHERE status = 'failed' AND updated_at < ?1 AND text_body IS NOT NULL",
            vec![(now - self.inner.cfg.failed_body_retention_ms).into()],
        )
        .await?;
        tx.exec(
            "INSERT INTO _forgebase_email_deliveries (id, realm, template, recipient, subject, status, attempts,
                 text_body, html_body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', 0, ?6, ?7, ?8, ?8)",
            vec![
                id.clone().into(),
                realm.clone().into(),
                input.template.into(),
                to.into(),
                p.subject.into(),
                p.text.into(),
                p.html.into(),
                now.into(),
            ],
        )
        .await?;
        tx.commit().await?;
        self.enqueue(&id).await?;
        self.delivery_unchecked(&id).await
    }

    async fn enqueue(&self, delivery_id: &str) -> Result<()> {
        let Some(jobs) = self.inner.jobs.get() else {
            return Err(CustomizeError::conflict(
                "mail_queue_unavailable",
                "the job queue is not attached; mail cannot be queued",
            ));
        };
        let mut new = NewJob::new(EMAIL_JOB, json!({"delivery_id": delivery_id}));
        new.max_attempts = Some(self.inner.cfg.email_max_attempts);
        new.backoff_base_ms = Some(self.inner.cfg.email_backoff_base_ms);
        new.idempotency_key = Some(format!("email:{delivery_id}:{}", new_id("n")));
        let (job, _) = jobs
            .enqueue(&AccessContext::system(), new)
            .await
            .map_err(|e| CustomizeError::Internal(format!("queueing mail: {e}")))?;
        let mut tx = self.begin().await?;
        tx.exec(
            "UPDATE _forgebase_email_deliveries SET job_id = ?2 WHERE id = ?1",
            vec![delivery_id.into(), job.id.into()],
        )
        .await?;
        tx.commit().await
    }

    /// One delivery attempt, called by the queue job. `attempt` counts from 1;
    /// `last` is true on the final attempt the queue allows.
    pub async fn deliver(&self, delivery_id: &str, attempt: i64, last: bool) -> Result<Attempt> {
        let rows = self
            .query(
                "SELECT realm, recipient, subject, status, text_body, html_body FROM _forgebase_email_deliveries WHERE id = ?1",
                vec![delivery_id.into()],
            )
            .await?;
        let Some(r) = rows.rows.first() else {
            return Ok(Attempt::Failed(format!("delivery {delivery_id} does not exist")));
        };
        if text(r, 3) == "sent" {
            return Ok(Attempt::Sent);
        }
        let realm = text(r, 0);
        let (Some(body), html) = (opt_text(r, 4), opt_text(r, 5)) else {
            self.finish(
                delivery_id,
                DeliveryStatus::Failed,
                attempt,
                Some("the message body is no longer stored"),
                "none",
                None,
            )
            .await?;
            return Ok(Attempt::Failed("the message body is no longer stored".into()));
        };
        let branding = self.branding_of(&realm).await?;
        let display = branding.display_name.unwrap_or_default();
        let (provider, from_override) = self.mail_for(&realm);
        let from_address = from_override.unwrap_or_else(|| self.inner.cfg.from_address.clone());
        let mail = OutMail {
            id: delivery_id.to_owned(),
            realm,
            to: text(r, 1),
            from: from_header(&display, &from_address),
            subject: text(r, 2),
            text: body,
            html,
            // Replies go to the realm's support address when it has one.
            reply_to: branding.support_email.filter(|e| valid_address(e)),
        };
        let name = provider.name();
        match provider.send(&mail).await {
            Ok(receipt) => {
                self.finish(
                    delivery_id,
                    DeliveryStatus::Sent,
                    attempt,
                    None,
                    name,
                    receipt.message_id.as_deref(),
                )
                .await?;
                Ok(Attempt::Sent)
            }
            Err(SendError::Transient(m)) if !last => {
                self.finish(delivery_id, DeliveryStatus::Retrying, attempt, Some(&m), name, None)
                    .await?;
                Ok(Attempt::Retry(m))
            }
            Err(SendError::Transient(m) | SendError::Permanent(m)) => {
                self.finish(delivery_id, DeliveryStatus::Failed, attempt, Some(&m), name, None)
                    .await?;
                Ok(Attempt::Failed(m))
            }
        }
    }

    async fn finish(
        &self,
        id: &str,
        status: DeliveryStatus,
        attempt: i64,
        error: Option<&str>,
        provider: &str,
        message_id: Option<&str>,
    ) -> Result<()> {
        let now = self.now();
        let mut tx = self.begin().await?;
        let sql = if status == DeliveryStatus::Sent {
            "UPDATE _forgebase_email_deliveries SET status = ?2, attempts = ?3, last_error = ?4, provider = ?5,
                 updated_at = ?6, sent_at = ?6, text_body = NULL, html_body = NULL,
                 provider_message_id = ?7 WHERE id = ?1"
        } else {
            "UPDATE _forgebase_email_deliveries SET status = ?2, attempts = ?3, last_error = ?4, provider = ?5,
                 updated_at = ?6, provider_message_id = COALESCE(?7, provider_message_id) WHERE id = ?1"
        };
        tx.exec(
            sql,
            vec![
                id.into(),
                status.as_str().into(),
                attempt.into(),
                error.map(|e| e.chars().take(500).collect::<String>()).into(),
                provider.into(),
                now.into(),
                message_id.map(|m| m.chars().take(300).collect::<String>()).into(),
            ],
        )
        .await?;
        tx.commit().await
    }

    async fn delivery_unchecked(&self, id: &str) -> Result<Delivery> {
        let rows = self
            .query(
                &format!("SELECT {DELIVERY_COLUMNS} FROM _forgebase_email_deliveries WHERE id = ?1"),
                vec![id.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| delivery_row(r))
            .ok_or_else(|| CustomizeError::NotFound("delivery".into()))
    }

    /// One delivery-log row.
    pub async fn delivery(&self, ctx: &AccessContext, id: &str) -> Result<Delivery> {
        let d = self.delivery_unchecked(id).await?;
        self.can_read(ctx, &d.realm)
            .map_err(|_| CustomizeError::NotFound("delivery".into()))?;
        Ok(d)
    }

    /// The delivery log of a realm, newest first.
    pub async fn deliveries(
        &self,
        ctx: &AccessContext,
        realm: Option<&str>,
        q: DeliveryQuery,
    ) -> Result<Vec<Delivery>> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        let mut sql = format!("SELECT {DELIVERY_COLUMNS} FROM _forgebase_email_deliveries WHERE realm = ?1");
        let mut params: Vec<Value> = vec![realm.into()];
        if let Some(s) = q.status {
            params.push(s.as_str().into());
            sql.push_str(&format!(" AND status = ?{}", params.len()));
        }
        if let Some(t) = q.to {
            params.push(t.into());
            sql.push_str(&format!(" AND recipient = ?{}", params.len()));
        }
        if let Some(t) = q.template {
            params.push(t.into());
            sql.push_str(&format!(" AND template = ?{}", params.len()));
        }
        if let Some(b) = q.before {
            params.push(b.into());
            sql.push_str(&format!(" AND created_at < ?{}", params.len()));
        }
        params.push(q.limit.unwrap_or(50).clamp(1, 200).into());
        sql.push_str(&format!(" ORDER BY created_at DESC, id DESC LIMIT ?{}", params.len()));
        let rows = self.query(&sql, params).await?;
        Ok(rows.rows.iter().map(|r| delivery_row(r)).collect())
    }

    /// Queues another round of attempts for a failed delivery whose body is
    /// still stored.
    pub async fn retry_delivery(&self, ctx: &AccessContext, id: &str) -> Result<Delivery> {
        let d = self.delivery_unchecked(id).await?;
        self.can_write(ctx, &d.realm)
            .map_err(|_| CustomizeError::NotFound("delivery".into()))?;
        if d.status != DeliveryStatus::Failed {
            return Err(CustomizeError::conflict(
                "not_failed",
                "only failed deliveries can be retried",
            ));
        }
        let rows = self
            .query(
                "SELECT text_body IS NOT NULL FROM _forgebase_email_deliveries WHERE id = ?1",
                vec![id.into()],
            )
            .await?;
        if rows.rows.first().map(|r| int(r, 0)).unwrap_or(0) == 0 {
            return Err(CustomizeError::conflict(
                "body_expired",
                "the message body is no longer stored; send it again",
            ));
        }
        let mut tx = self.begin().await?;
        tx.exec(
            "UPDATE _forgebase_email_deliveries SET status = 'queued', attempts = 0, last_error = NULL, updated_at = ?2 WHERE id = ?1",
            vec![id.into(), self.now().into()],
        )
        .await?;
        tx.commit().await?;
        self.enqueue(id).await?;
        self.delivery_unchecked(id).await
    }
}
