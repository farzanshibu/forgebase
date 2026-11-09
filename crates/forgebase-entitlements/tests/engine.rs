//! Entitlements and quotas through the Rust API and the FB-21 pipeline, on
//! real SQLite, with an injected clock (no sleeps for period logic).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use forgebase_entitlements::period::utc;
use forgebase_entitlements::{
    AddonFeature, AddonInput, AddonQuantity, CustomLimit, EntitlementError, Enforcement, FeatureKind, GaugeRequest,
    GrantSource, OverrideInput, PackGrantInput, QuotaPolicy, QuotaScope, ReservationStatus,
    SubscriptionStatus, TrialInput, UsagePackInput,
};
use forgebase_jobs::{JobRegistry, PoolConfig, start_pool};
use forgebase_policy::{Check, PolicyError, Step};

fn ai_check(org: &str) -> Check {
    Check {
        permission: "ai:generate".into(),
        org: Some(org.to_owned()),
        ..Check::default()
    }
}

fn denied_at(r: Result<(), PolicyError>) -> Option<Step> {
    match r {
        Err(PolicyError::Denied(d)) => Some(d.step),
        _ => None,
    }
}

fn exceeded(r: &Result<impl std::fmt::Debug, EntitlementError>) -> bool {
    matches!(r, Err(EntitlementError::QuotaExceeded(_)))
}

/// An org `acme` (owner alice, member bob) and the `ai_generation` feature
/// gating `ai:*`, admitted at step 6 for `ai:generate`.
async fn ai_env() -> (Env, String) {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    env.member("alice", &org, "bob", &["member"]).await;
    env.feature("ai_generation", FeatureKind::Metered, &["ai:*"]).await;
    env.policy_for(
        "ai_generation",
        QuotaPolicy {
            permissions: vec!["ai:generate".into()],
            ..QuotaPolicy::default()
        },
    )
    .await;
    (env, org)
}

#[tokio::test]
async fn entitlement_denies_when_the_feature_is_off_even_with_quota_left() {
    let (env, org) = ai_env().await;
    let mut off = pf("ai_generation", Some(100), Some("month"));
    off.enabled = false;
    env.plan("free", vec![off]).await;
    env.subscribe(&org_subject(&org), "free").await;
    let bob = user("bob").with_org(&org);

    assert_eq!(denied_at(env.policy.check(&bob, &ai_check(&org))), Some(Step::Entitlement));
    let r = env
        .ent
        .consume(&sys(), req("ai_generation", 1, "k").for_subject(org_subject(&org)))
        .await;
    assert!(matches!(r, Err(EntitlementError::FeatureDisabled { .. })), "{r:?}");
    assert!(!env.ent.has_feature(&bob, "ai_generation", None).unwrap().has);
    let has = env.ent.has_permission(&bob, "ai:generate").unwrap();
    assert!(!has.has);
    assert!(has.reason.unwrap().contains("ai_generation"));
    // org:<feature>:<permission> keys are tied to the feature too.
    env.feature("projects", FeatureKind::Boolean, &[]).await;
    let mut no_projects = pf("projects", None, None);
    no_projects.enabled = false;
    env.plan("free", vec![no_projects]).await;
    let c = Check {
        permission: "org:projects:create".into(),
        org: Some(org.clone()),
        ..Check::default()
    };
    assert_eq!(denied_at(env.policy.check(&user("alice").with_org(&org), &c)), Some(Step::Entitlement));

    // Turning the feature on in the plan admits at once (snapshot swap).
    env.plan(
        "free",
        vec![pf("ai_generation", Some(100), Some("month")), pf("projects", None, None)],
    )
    .await;
    env.policy.check(&bob, &ai_check(&org)).unwrap();
    env.policy.check(&user("alice").with_org(&org), &c).unwrap();
    assert!(env.ent.has_permission(&bob, "ai:generate").unwrap().has);
}

#[tokio::test]
async fn quota_denies_when_exhausted_even_with_the_feature_on() {
    let (env, org) = ai_env().await;
    env.plan("pro", vec![pf("ai_generation", Some(3), Some("month"))]).await;
    let subject = org_subject(&org);
    env.subscribe(&subject, "pro").await;
    let bob = user("bob").with_org(&org);
    for i in 0..3 {
        env.policy.check(&bob, &ai_check(&org)).unwrap();
        let c = env
            .ent
            .consume(&sys(), req("ai_generation", 1, &format!("k{i}")).for_subject(&subject))
            .await
            .unwrap();
        assert_eq!(c.decision.remaining, Some(2 - i));
    }
    let r = env
        .ent
        .consume(&sys(), req("ai_generation", 1, "k3").for_subject(&subject))
        .await;
    match r {
        Err(EntitlementError::QuotaExceeded(d)) => {
            assert_eq!((d.used, d.limit, d.remaining), (3, Some(3), Some(0)));
            assert!(!d.allowed);
        }
        other => panic!("{other:?}"),
    }
    // The feature is still entitled; step 6 denies.
    assert!(env.ent.has_feature(&bob, "ai_generation", None).unwrap().has);
    assert_eq!(denied_at(env.policy.check(&bob, &ai_check(&org))), Some(Step::Quota));
    // The dry run agrees and changes nothing.
    let d = env
        .ent
        .check(&user("alice").with_org(&org), &req("ai_generation", 1, "x"))
        .await
        .unwrap();
    assert!(!d.allowed);
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_usage_events").await, 3);
    // Next month the hook admits again (cache knows the window ended).
    env.clock.set(utc(2026, 12, 1, 0, 0));
    env.policy.check(&bob, &ai_check(&org)).unwrap();
}

#[tokio::test]
async fn fifty_concurrent_reservations_against_ten_grant_exactly_ten() {
    let (env, org) = ai_env().await;
    env.plan("pro", vec![pf("ai_generation", Some(10), Some("month"))]).await;
    let subject = org_subject(&org);
    env.subscribe(&subject, "pro").await;
    let mut tasks = Vec::new();
    for i in 0..50 {
        let ent = env.ent.clone();
        let subject = subject.clone();
        tasks.push(tokio::spawn(async move {
            ent.reserve(&sys(), req("ai_generation", 1, &format!("r{i}")).for_subject(subject))
                .await
        }));
    }
    let mut granted = Vec::new();
    let mut refused = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(r) => granted.push(r),
            Err(EntitlementError::QuotaExceeded(_)) => refused += 1,
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!((granted.len(), refused), (10, 40));
    assert_eq!(
        env.count("SELECT reserved FROM _forgebase_usage_counters WHERE feature_key = 'ai_generation'")
            .await,
        10
    );
    // Concurrent commits settle each exactly once.
    let mut tasks = Vec::new();
    for r in granted {
        let ent = env.ent.clone();
        tasks.push(tokio::spawn(async move { ent.commit(&sys(), &r.id, None).await }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap().status, ReservationStatus::Committed);
    }
    assert_eq!(
        env.count("SELECT used * 100 + reserved FROM _forgebase_usage_counters WHERE feature_key = 'ai_generation'")
            .await,
        1000
    );
    assert_eq!(env.count("SELECT COALESCE(SUM(units), 0) FROM _forgebase_usage_events").await, 10);
    // Concurrent one-shot consumption never over-grants either.
    env.clock.set(utc(2026, 12, 1, 0, 0));
    let mut tasks = Vec::new();
    for i in 0..50 {
        let ent = env.ent.clone();
        let subject = subject.clone();
        tasks.push(tokio::spawn(async move {
            ent.consume(&sys(), req("ai_generation", 1, &format!("c{i}")).for_subject(subject))
                .await
                .is_ok()
        }));
    }
    let mut ok = 0;
    for t in tasks {
        ok += i32::from(t.await.unwrap());
    }
    assert_eq!(ok, 10);
}

#[tokio::test]
async fn idempotent_replay_returns_the_first_result() {
    let (env, org) = ai_env().await;
    env.feature("storage", FeatureKind::Gauge, &[]).await;
    env.feature("runners", FeatureKind::Concurrency, &[]).await;
    env.plan(
        "pro",
        vec![
            pf("ai_generation", Some(5), Some("month")),
            pf("storage", Some(100), None),
            pf("runners", Some(1), None),
        ],
    )
    .await;
    let s = org_subject(&org);
    env.subscribe(&s, "pro").await;

    let first = env
        .ent
        .consume(&sys(), req("ai_generation", 2, "same").for_subject(&s))
        .await
        .unwrap();
    let again = env
        .ent
        .consume(&sys(), req("ai_generation", 2, "same").for_subject(&s))
        .await
        .unwrap();
    assert!(!first.replayed && again.replayed);
    assert_eq!(first.event_id, again.event_id);
    assert_eq!(first.decision, again.decision);
    assert_eq!(env.count("SELECT used FROM _forgebase_usage_counters").await, 2);
    let r = env
        .ent
        .consume(&sys(), req("ai_generation", 3, "same").for_subject(&s))
        .await;
    assert!(matches!(r, Err(EntitlementError::Conflict { code: "idempotency_conflict", .. })), "{r:?}");
    // Keys are per subject: another subject may use the same key.
    env.subscribe(&pk("alice"), "pro").await;
    let other = env
        .ent
        .consume(&sys(), req("ai_generation", 2, "same").for_subject(pk("alice")))
        .await
        .unwrap();
    assert!(!other.replayed);

    let r1 = env
        .ent
        .reserve(&sys(), req("ai_generation", 1, "res").for_subject(&s))
        .await
        .unwrap();
    let r2 = env
        .ent
        .reserve(&sys(), req("ai_generation", 1, "res").for_subject(&s))
        .await
        .unwrap();
    assert_eq!(r1.id, r2.id);
    assert!(r2.replayed);
    let c1 = env.ent.commit(&sys(), &r1.id, None).await.unwrap();
    let c2 = env.ent.commit(&sys(), &r1.id, None).await.unwrap();
    assert_eq!(c1.status, ReservationStatus::Committed);
    assert!(c2.replayed);
    assert_eq!(env.count("SELECT used FROM _forgebase_usage_counters WHERE subject LIKE 'org:%'").await, 3);
    let r = env.ent.release(&sys(), &r1.id).await;
    assert!(matches!(r, Err(EntitlementError::Conflict { code: "reservation_settled", .. })));

    let g = GaugeRequest {
        feature: "storage".into(),
        delta: Some(40),
        subject: Some(s.clone()),
        idempotency_key: Some("upload-1".into()),
        ..GaugeRequest::default()
    };
    let a = env.ent.adjust_gauge(&sys(), g.clone()).await.unwrap();
    let b = env.ent.adjust_gauge(&sys(), g).await.unwrap();
    assert_eq!(a.event_id, b.event_id);
    assert_eq!(b.decision.used, 40);

    let l1 = env
        .ent
        .acquire_lease(&sys(), req("runners", 1, "job-1").for_subject(&s))
        .await
        .unwrap();
    let l2 = env
        .ent
        .acquire_lease(&sys(), req("runners", 1, "job-1").for_subject(&s))
        .await
        .unwrap();
    assert_eq!(l1.id, l2.id);
    assert!(l2.replayed);
}

#[tokio::test]
async fn every_period_resets_at_its_boundary() {
    let env = Env::new().await;
    for f in ["per_hour", "per_day", "per_month", "ever"] {
        env.feature(f, FeatureKind::Metered, &[]).await;
    }
    env.plan(
        "p",
        vec![
            pf("per_hour", Some(2), Some("hour")),
            pf("per_day", Some(2), Some("day")),
            pf("per_month", Some(2), Some("month")),
            pf("ever", Some(2), Some("lifetime")),
        ],
    )
    .await;
    let s = pk("alice");
    env.subscribe(&s, "p").await;
    let mut n = 0;
    let mut use_up = async |env: &Env, f: &str| -> bool {
        let mut ok = true;
        for _ in 0..2 {
            n += 1;
            ok &= env
                .ent
                .consume(&sys(), req(f, 1, &format!("k{n}")).for_subject(&s))
                .await
                .is_ok();
        }
        n += 1;
        let over = env
            .ent
            .consume(&sys(), req(f, 1, &format!("k{n}")).for_subject(&s))
            .await;
        ok && exceeded(&over)
    };
    // T0 = 2026-11-09 12:00.
    for f in ["per_hour", "per_day", "per_month", "ever"] {
        assert!(use_up(&env, f).await, "{f}");
    }
    env.clock.set(utc(2026, 11, 9, 12, 59));
    assert!(exceeded(&env.ent.consume(&sys(), req("per_hour", 1, "h1").for_subject(&s)).await));
    env.clock.set(utc(2026, 11, 9, 13, 0));
    env.ent.run_maintenance().await.unwrap();
    assert!(use_up(&env, "per_hour").await, "hour reset");
    assert!(exceeded(&env.ent.consume(&sys(), req("per_day", 1, "d1").for_subject(&s)).await));
    env.clock.set(utc(2026, 11, 10, 0, 0));
    assert!(use_up(&env, "per_day").await, "day reset");
    assert!(exceeded(&env.ent.consume(&sys(), req("per_month", 1, "m1").for_subject(&s)).await));
    env.clock.set(utc(2026, 12, 1, 0, 0));
    assert!(use_up(&env, "per_month").await, "month reset");
    env.clock.set(utc(2028, 1, 1, 0, 0));
    assert!(exceeded(&env.ent.consume(&sys(), req("ever", 1, "e1").for_subject(&s)).await), "lifetime never resets");

    // Billing-cycle months: anchored on the 15th at 08:00.
    let s2 = pk("bob");
    env.clock.set(T0);
    env.subscribe_with(&s2, "p", |x| x.cycle_anchor = Some(utc(2026, 1, 15, 8, 0))).await;
    for i in 0..2 {
        env.ent
            .consume(&sys(), req("per_month", 1, &format!("b{i}")).for_subject(&s2))
            .await
            .unwrap();
    }
    let d = env.ent.check(&sys(), &req("per_month", 1, "x").for_subject(&s2)).await.unwrap();
    assert!(!d.allowed);
    assert_eq!(d.window_start, utc(2026, 10, 15, 8, 0));
    assert_eq!(d.window_end, Some(utc(2026, 11, 15, 8, 0)));
    env.clock.set(utc(2026, 11, 15, 7, 59));
    assert!(!env.ent.check(&sys(), &req("per_month", 1, "x").for_subject(&s2)).await.unwrap().allowed);
    env.clock.set(utc(2026, 11, 15, 8, 0));
    assert!(env.ent.check(&sys(), &req("per_month", 1, "x").for_subject(&s2)).await.unwrap().allowed);
}

#[tokio::test]
async fn rolling_windows_slide() {
    let env = Env::new().await;
    env.feature("calls", FeatureKind::Metered, &[]).await;
    let mut f = pf("calls", Some(3), Some("rolling"));
    f.window_secs = Some(3600);
    env.plan("p", vec![f]).await;
    let s = pk("alice");
    env.subscribe(&s, "p").await;
    for (i, at) in [0, 10, 20].into_iter().enumerate() {
        env.clock.set(T0 + at * MIN);
        env.ent
            .consume(&sys(), req("calls", 1, &format!("k{i}")).for_subject(&s))
            .await
            .unwrap();
    }
    env.clock.set(T0 + 30 * MIN);
    assert!(exceeded(&env.ent.consume(&sys(), req("calls", 1, "k3").for_subject(&s)).await));
    // Exactly one hour after the first call it leaves the window.
    env.clock.set(T0 + HOUR - 1);
    assert!(exceeded(&env.ent.consume(&sys(), req("calls", 1, "k4").for_subject(&s)).await));
    env.clock.set(T0 + HOUR);
    env.ent.consume(&sys(), req("calls", 1, "k5").for_subject(&s)).await.unwrap();
    assert!(exceeded(&env.ent.consume(&sys(), req("calls", 1, "k6").for_subject(&s)).await));
    env.clock.set(T0 + HOUR + 20 * MIN);
    let d = env.ent.check(&sys(), &req("calls", 2, "x").for_subject(&s)).await.unwrap();
    assert!(d.allowed, "{d:?}");
    assert_eq!(d.used, 1);
}

#[tokio::test]
async fn unused_allowance_rolls_over_capped() {
    let env = Env::new().await;
    env.feature("credits", FeatureKind::Metered, &[]).await;
    env.policy_for(
        "credits",
        QuotaPolicy {
            rollover_pct: 50,
            rollover_cap: Some(30),
            ..QuotaPolicy::default()
        },
    )
    .await;
    env.plan("p", vec![pf("credits", Some(100), Some("month"))]).await;
    let (busy, idle) = (pk("alice"), pk("bob"));
    env.subscribe(&busy, "p").await;
    env.subscribe(&idle, "p").await;
    env.ent.run_maintenance().await.unwrap(); // opens November windows
    env.ent.consume(&sys(), req("credits", 40, "nov").for_subject(&busy)).await.unwrap();

    // December: 60 unused, at most 50 % of 100 and at most 30 → 30 carried.
    env.clock.set(utc(2026, 12, 1, 0, 0));
    let report = env.ent.run_maintenance().await.unwrap();
    assert_eq!(report.windows_opened, 2);
    let d = env.ent.check(&sys(), &req("credits", 1, "x").for_subject(&busy)).await.unwrap();
    assert_eq!(d.limit, Some(130));
    let d = env.ent.check(&sys(), &req("credits", 1, "x").for_subject(&idle)).await.unwrap();
    assert_eq!(d.limit, Some(130), "an idle month rolls over too");
    env.ent.consume(&sys(), req("credits", 130, "dec").for_subject(&busy)).await.unwrap();
    assert!(exceeded(&env.ent.consume(&sys(), req("credits", 1, "dec2").for_subject(&busy)).await));
    // January: everything was used in December → nothing carried; never compounding.
    env.clock.set(utc(2027, 1, 1, 0, 0));
    env.ent.run_maintenance().await.unwrap();
    let d = env.ent.check(&sys(), &req("credits", 1, "x").for_subject(&busy)).await.unwrap();
    assert_eq!(d.limit, Some(100));
    let d = env.ent.check(&sys(), &req("credits", 1, "x").for_subject(&idle)).await.unwrap();
    assert_eq!(d.limit, Some(130));
}

#[tokio::test]
async fn soft_limits_record_overage_and_hard_limits_deny() {
    let env = Env::new().await;
    for f in ["hard_f", "soft_f", "open_f"] {
        env.feature(f, FeatureKind::Metered, &[]).await;
    }
    env.policy_for(
        "soft_f",
        QuotaPolicy {
            enforcement: Enforcement::Soft,
            overage_pct: Some(50),
            ..QuotaPolicy::default()
        },
    )
    .await;
    env.policy_for(
        "open_f",
        QuotaPolicy {
            enforcement: Enforcement::Soft,
            ..QuotaPolicy::default()
        },
    )
    .await;
    env.plan(
        "p",
        vec![
            pf("hard_f", Some(10), Some("month")),
            pf("soft_f", Some(10), Some("month")),
            pf("open_f", Some(10), Some("month")),
        ],
    )
    .await;
    let s = pk("alice");
    env.subscribe(&s, "p").await;
    env.ent.consume(&sys(), req("hard_f", 10, "h1").for_subject(&s)).await.unwrap();
    assert!(exceeded(&env.ent.consume(&sys(), req("hard_f", 1, "h2").for_subject(&s)).await));

    env.ent.consume(&sys(), req("soft_f", 8, "s1").for_subject(&s)).await.unwrap();
    let c = env.ent.consume(&sys(), req("soft_f", 7, "s2").for_subject(&s)).await.unwrap();
    assert_eq!(c.decision.overage_units, 5);
    assert_eq!(c.decision.used, 15);
    assert!(exceeded(&env.ent.consume(&sys(), req("soft_f", 1, "s3").for_subject(&s)).await), "ceiling 150 %");
    assert_eq!(
        env.count("SELECT SUM(overage_units) FROM _forgebase_usage_events WHERE feature_key = 'soft_f'").await,
        5
    );
    let c = env.ent.consume(&sys(), req("open_f", 1000, "o1").for_subject(&s)).await.unwrap();
    assert_eq!(c.decision.overage_units, 990);
}

#[tokio::test]
async fn alerts_fire_once_per_threshold_and_are_delivered_by_the_job() {
    let env = Env::new().await;
    env.feature("ai", FeatureKind::Metered, &[]).await;
    env.plan("p", vec![pf("ai", Some(10), Some("month"))]).await;
    let s = pk("alice");
    env.subscribe(&s, "p").await;
    for (i, units) in [7, 1, 1, 1].into_iter().enumerate() {
        env.ent
            .consume(&sys(), req("ai", units, &format!("k{i}")).for_subject(&s))
            .await
            .unwrap();
    }
    assert!(exceeded(&env.ent.consume(&sys(), req("ai", 1, "k9").for_subject(&s)).await));
    let alerts = env.ent.alerts(&sys(), Some(&s)).await.unwrap();
    let mut t: Vec<i64> = alerts.iter().map(|a| a.threshold).collect();
    t.sort_unstable();
    assert_eq!(t, vec![70, 90, 100]);
    // A reservation commit that crosses thresholds raises them too, next window.
    env.clock.set(utc(2026, 12, 1, 0, 0));
    let r = env.ent.reserve(&sys(), req("ai", 10, "r").for_subject(&s)).await.unwrap();
    assert_eq!(env.ent.alerts(&sys(), Some(&s)).await.unwrap().len(), 3, "holds raise no alert");
    env.ent.commit(&sys(), &r.id, None).await.unwrap();
    assert_eq!(env.ent.alerts(&sys(), Some(&s)).await.unwrap().len(), 6);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_jobs WHERE kind = 'entitlements.alert'").await,
        6
    );

    let registry = forgebase_entitlements::register_jobs(JobRegistry::new(), env.ent.clone());
    let pool = start_pool(
        env.jobs.clone(),
        registry,
        PoolConfig {
            poll_interval: Duration::from_millis(20),
            maintenance_interval: Duration::from_millis(50),
            ..PoolConfig::default()
        },
    );
    for _ in 0..250 {
        if env.sink.0.lock().unwrap().len() >= 6 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    pool.shutdown().await;
    assert_eq!(env.sink.0.lock().unwrap().len(), 6);
    assert!(env.ent.alerts(&sys(), Some(&s)).await.unwrap().iter().all(|a| a.delivered_at.is_some()));
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_entitlement_audit WHERE action = 'usage_alert'").await,
        6
    );
    let id = env.ent.alerts(&sys(), Some(&s)).await.unwrap()[0].id.clone();
    assert!(!env.ent.deliver_alert(&id).await.unwrap(), "delivered once");
    assert_eq!(env.sink.0.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn overrides_and_enterprise_limits_win_in_priority_order() {
    let (env, org) = ai_env().await;
    let s = org_subject(&org);
    env.plan("pro", vec![pf("ai_generation", Some(100), Some("month"))]).await;
    env.ent
        .put_addon(
            &admin(),
            "ai_boost",
            AddonInput {
                name: None,
                description: None,
                active: true,
                features: vec![AddonFeature {
                    feature: "ai_generation".into(),
                    limit: Some(50),
                    period: None,
                    window_secs: None,
                }],
            },
        )
        .await
        .unwrap();
    env.subscribe_with(&s, "pro", |x| {
        x.addons = Some(vec![AddonQuantity {
            addon: "ai_boost".into(),
            quantity: 2,
        }]);
    })
    .await;
    let limit = |member: Option<&str>| {
        let e = env.ent.entitlements_of(&s, member);
        e.features.get("ai_generation").map(|g| (g.enabled, g.limit, g.source))
    };
    // 5 org plan (+ add-ons).
    assert_eq!(limit(None), Some((true, Some(200), GrantSource::Addon)));
    // Enterprise contract replaces the plan grant.
    env.ent
        .put_custom_limit(
            &admin(),
            CustomLimit {
                subject: s.clone(),
                feature: "ai_generation".into(),
                enabled: true,
                limit: Some(1000),
                period: None,
                window_secs: None,
                reason: "contract 42".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(limit(None), Some((true, Some(1000), GrantSource::Custom)));
    let ov = |subject: Option<&str>, member: Option<&str>, effect: &str, limit: Option<i64>| OverrideInput {
        subject: subject.map(str::to_owned),
        member: member.map(str::to_owned),
        feature: "ai_generation".into(),
        effect: effect.into(),
        limit,
        period: None,
        window_secs: None,
        expires_at: None,
        reason: String::new(),
    };
    // 7 quota overrides: subject-wide, then the member inside the subject.
    env.ent.add_override(&admin(), ov(Some(&s), None, "allow", Some(5000))).await.unwrap();
    assert_eq!(limit(None), Some((true, Some(5000), GrantSource::Override)));
    env.ent
        .add_override(&admin(), ov(Some(&s), Some(&pk("bob")), "allow", Some(10)))
        .await
        .unwrap();
    assert_eq!(limit(Some(&pk("bob"))).unwrap().1, Some(10));
    assert_eq!(limit(Some(&pk("alice"))).unwrap().1, Some(5000));
    // 2 explicit deny beats every grant, including the enterprise limit.
    let deny = env
        .ent
        .add_override(&admin(), ov(None, Some(&pk("bob")), "deny", None))
        .await
        .unwrap();
    assert!(!limit(Some(&pk("bob"))).unwrap().0);
    assert!(limit(Some(&pk("alice"))).unwrap().0);
    let bob = user("bob").with_org(&org);
    assert_eq!(denied_at(env.policy.check(&bob, &ai_check(&org))), Some(Step::Entitlement));
    env.policy.check(&user("alice").with_org(&org), &ai_check(&org)).unwrap();
    env.ent.delete_override(&admin(), &deny.id).await.unwrap();
    env.policy.check(&bob, &ai_check(&org)).unwrap();
    // A subject-wide deny disables the feature for every member.
    let d2 = env.ent.add_override(&admin(), ov(Some(&s), None, "deny", None)).await.unwrap();
    assert!(!limit(Some(&pk("alice"))).unwrap().0);
    env.ent.delete_override(&admin(), &d2.id).await.unwrap();
    // Same specificity: the newest override applies. Expired overrides stop
    // applying without a reload.
    env.clock.set(T0 + MIN);
    let mut temp = ov(Some(&s), None, "allow", Some(7));
    temp.expires_at = Some(T0 + HOUR);
    env.ent.add_override(&admin(), temp).await.unwrap();
    assert_eq!(limit(None).unwrap().1, Some(7));
    env.clock.set(T0 + HOUR);
    assert_eq!(limit(None).unwrap().1, Some(5000));
    // The user-scope quota of a member is enforced at that member's limit.
    env.policy_for(
        "ai_generation",
        QuotaPolicy {
            scope: QuotaScope::User,
            ..QuotaPolicy::default()
        },
    )
    .await;
    let r = env
        .ent
        .consume(&sys(), req("ai_generation", 11, "b").for_subject(&s).scoped(pk("bob")))
        .await;
    assert!(exceeded(&r));
    env.ent
        .consume(&sys(), req("ai_generation", 10, "b2").for_subject(&s).scoped(pk("bob")))
        .await
        .unwrap();
    env.ent
        .consume(&sys(), req("ai_generation", 100, "a").for_subject(&s).scoped(pk("alice")))
        .await
        .unwrap();
}

#[tokio::test]
async fn org_and_personal_subjects_are_billed_separately() {
    let (env, org) = ai_env().await;
    let mut off = pf("ai_generation", Some(0), Some("month"));
    off.enabled = false;
    env.plan("free", vec![off]).await;
    env.plan("pro", vec![pf("ai_generation", Some(5), Some("month"))]).await;
    env.subscribe(&pk("alice"), "pro").await;
    env.subscribe(&org_subject(&org), "free").await;
    let personal = Check {
        permission: "ai:generate".into(),
        owner: Some(pk("alice")),
        ..Check::default()
    };
    env.policy.check(&user("alice"), &personal).unwrap();
    assert_eq!(
        denied_at(env.policy.check(&user("alice").with_org(&org), &ai_check(&org))),
        Some(Step::Entitlement),
        "in the org, the org's plan decides"
    );
    assert!(env.ent.has_feature(&user("alice"), "ai_generation", None).unwrap().has);
    assert!(!env.ent.has_feature(&user("alice").with_org(&org), "ai_generation", None).unwrap().has);
    let e = env.ent.entitlements(&user("alice").with_org(&org), None).unwrap();
    assert_eq!((e.subject.as_str(), e.plan.as_deref()), (org_subject(&org).as_str(), Some("free")));

    // Usage lands on the subject of the check.
    env.plan("free", vec![pf("ai_generation", Some(3), Some("month"))]).await;
    env.ent.consume(&sys(), req("ai_generation", 3, "o").for_subject(org_subject(&org))).await.unwrap();
    env.ent.consume(&sys(), req("ai_generation", 2, "p").for_subject(pk("alice"))).await.unwrap();
    let org_usage = env.ent.usage(&user("alice").with_org(&org), None, None).await.unwrap();
    let mine = env.ent.usage(&user("alice"), None, None).await.unwrap();
    assert_eq!((org_usage[0].used, org_usage[0].limit), (3, Some(3)));
    assert_eq!((mine[0].used, mine[0].limit), (2, Some(5)));
    // The default plan covers subjects without a subscription.
    env.plan_with("starter", true, 0, vec![pf("ai_generation", Some(1), Some("day"))]).await;
    let e = env.ent.entitlements(&user("carol"), None).unwrap();
    assert_eq!(e.plan.as_deref(), Some("starter"));
    assert!(!e.entitled);
}

#[tokio::test]
async fn subjects_are_isolated_from_each_other() {
    let (env, org1) = ai_env().await;
    let org2 = env.org("carol", "globex").await;
    env.plan("pro", vec![pf("ai_generation", Some(10), Some("month"))]).await;
    let (s1, s2) = (org_subject(&org1), org_subject(&org2));
    env.subscribe(&s1, "pro").await;
    env.subscribe(&s2, "pro").await;
    // alice owns org1 (holds every permission there, usage:write included).
    let alice = user("alice").with_org(&org1);
    env.ent.consume(&alice, req("ai_generation", 1, "mine")).await.unwrap();
    let theirs = env.ent.reserve(&sys(), req("ai_generation", 2, "r").for_subject(&s2)).await.unwrap();
    let lease_feature = env.ent.consume(&alice, req("ai_generation", 1, "x").for_subject(&s2)).await;
    assert!(matches!(lease_feature, Err(EntitlementError::Policy(PolicyError::Denied(_)))), "{lease_feature:?}");
    assert!(env.ent.usage(&alice, Some(&s2), None).await.is_err());
    assert!(env.ent.entitlements(&alice, Some(&s2)).is_err());
    assert!(env.ent.events(&alice, Some(&s2), None, None, None).await.is_err());
    assert!(env.ent.alerts(&alice, Some(&s2)).await.is_err());
    assert!(env.ent.check(&alice, &req("ai_generation", 1, "c").for_subject(&s2)).await.is_err());
    assert!(matches!(env.ent.commit(&alice, &theirs.id, None).await, Err(EntitlementError::NotFound(_))));
    assert!(matches!(env.ent.release(&alice, &theirs.id).await, Err(EntitlementError::NotFound(_))));
    // A plain member cannot meter (usage:write), not even their own org,
    // and a user cannot meter their own personal subject either.
    env.member("alice", &org1, "bob", &["member"]).await;
    assert!(env.ent.consume(&user("bob").with_org(&org1), req("ai_generation", 1, "b")).await.is_err());
    env.subscribe(&pk("bob"), "pro").await;
    assert!(env.ent.consume(&user("bob"), req("ai_generation", 1, "b")).await.is_err());
    // Members see their subject's entitlements but not another's.
    assert!(env.ent.entitlements(&user("bob").with_org(&org1), None).is_ok());
    assert!(env.ent.entitlements(&user("bob"), Some(&s2)).is_err());
    assert!(env.ent.entitlements(&user("bob"), Some(&pk("alice"))).is_err());
    // Anonymous callers get nothing.
    let anon = forgebase_data::AccessContext::anonymous();
    assert!(matches!(env.ent.entitlements(&anon, None), Err(EntitlementError::Unauthenticated)));
    // Catalog and subject administration is for platform administrators.
    let r = env.ent.put_custom_limit(
        &alice,
        CustomLimit {
            subject: s1.clone(),
            feature: "ai_generation".into(),
            enabled: true,
            limit: None,
            period: None,
            window_secs: None,
            reason: String::new(),
        },
    );
    assert!(matches!(r.await, Err(EntitlementError::Forbidden(_))));
    // org2 usage is untouched.
    let u = env.ent.usage(&admin(), Some(&s2), None).await.unwrap();
    assert_eq!((u[0].used, u[0].reserved), (0, 2));
}

#[tokio::test]
async fn reservations_commit_partially_release_and_expire() {
    let env = Env::new().await;
    env.feature("tokens", FeatureKind::Metered, &[]).await;
    env.plan("p", vec![pf("tokens", Some(100), Some("month"))]).await;
    let s = pk("alice");
    env.subscribe(&s, "p").await;
    let r = env.ent.reserve(&sys(), req("tokens", 60, "a").for_subject(&s)).await.unwrap();
    assert!(exceeded(&env.ent.reserve(&sys(), req("tokens", 50, "b").for_subject(&s)).await));
    let c = env.ent.commit(&sys(), &r.id, Some(25)).await.unwrap();
    assert_eq!(c.committed_units, Some(25));
    let d = env.ent.check(&sys(), &req("tokens", 1, "x").for_subject(&s)).await.unwrap();
    assert_eq!((d.used, d.reserved, d.remaining), (25, 0, Some(75)));
    let r2 = env.ent.reserve(&sys(), req("tokens", 75, "c").for_subject(&s)).await.unwrap();
    env.ent.release(&sys(), &r2.id).await.unwrap();
    let d = env.ent.check(&sys(), &req("tokens", 1, "x").for_subject(&s)).await.unwrap();
    assert_eq!((d.used, d.reserved), (25, 0));
    // A hold that is not settled in time is given back.
    let r3 = env.ent.reserve(&sys(), req("tokens", 75, "d").for_subject(&s).ttl(60)).await.unwrap();
    env.clock.advance(61_000);
    let late = env.ent.commit(&sys(), &r3.id, None).await;
    assert!(matches!(late, Err(EntitlementError::Conflict { code: "reservation_expired", .. })), "{late:?}");
    let report = env.ent.run_maintenance().await.unwrap();
    assert_eq!(report.reservations_expired, 1);
    let d = env.ent.check(&sys(), &req("tokens", 75, "x").for_subject(&s)).await.unwrap();
    assert!(d.allowed);
    assert_eq!(d.reserved, 0);
    // Expired holds are also reaped by the next reservation of the key.
    let r4 = env.ent.reserve(&sys(), req("tokens", 75, "e").for_subject(&s).ttl(1)).await.unwrap();
    env.clock.advance(2_000);
    env.ent.reserve(&sys(), req("tokens", 75, "f").for_subject(&s)).await.unwrap();
    let row = env.ent.release(&sys(), &r4.id).await;
    assert!(matches!(row, Err(EntitlementError::Conflict { code: "reservation_settled", .. })));
}

#[tokio::test]
async fn usage_packs_extend_the_allowance_and_trials_end() {
    let env = Env::new().await;
    env.feature("ai", FeatureKind::Metered, &[]).await;
    env.ent
        .put_usage_pack(
            &admin(),
            "ai_pack",
            UsagePackInput {
                name: None,
                feature: "ai".into(),
                units: 10,
                valid_days: Some(30),
                active: true,
            },
        )
        .await
        .unwrap();
    let mut pro = pf("ai", Some(5), Some("month"));
    pro.trial_limit = Some(2);
    env.plan_with("free", true, 0, vec![]).await;
    env.plan_with("pro", false, 14, vec![pro]).await;
    let s = pk("alice");
    env.subscribe(&s, "pro").await;
    let grant = PackGrantInput {
        subject: s.clone(),
        pack: "ai_pack".into(),
        quantity: 1,
        idempotency_key: Some("order-1".into()),
    };
    let g1 = env.ent.grant_pack(&admin(), grant.clone()).await.unwrap();
    let g2 = env.ent.grant_pack(&admin(), grant).await.unwrap();
    assert_eq!(g1.id, g2.id, "grants are idempotent");
    env.ent.consume(&sys(), req("ai", 5, "w").for_subject(&s)).await.unwrap();
    let c = env.ent.consume(&sys(), req("ai", 3, "p").for_subject(&s)).await.unwrap();
    assert_eq!((c.decision.pack_units, c.decision.pack_remaining), (3, 7));
    let r = env.ent.reserve(&sys(), req("ai", 4, "r").for_subject(&s)).await.unwrap();
    assert_eq!(r.pack_units, 4);
    env.ent.release(&sys(), &r.id).await.unwrap();
    assert_eq!(env.ent.pack_grants(&sys(), &s).await.unwrap()[0].remaining, 7);
    assert!(exceeded(&env.ent.consume(&sys(), req("ai", 8, "big").for_subject(&s)).await));
    // Packs survive the period reset, until they expire.
    env.clock.set(utc(2026, 12, 1, 0, 0));
    let d = env.ent.check(&sys(), &req("ai", 12, "x").for_subject(&s)).await.unwrap();
    assert!(d.allowed);
    env.clock.set(T0 + 31 * DAY);
    let d = env.ent.check(&sys(), &req("ai", 6, "x").for_subject(&s)).await.unwrap();
    assert!(!d.allowed);
    assert_eq!(d.pack_remaining, 0);

    // Trials: the trial allowance applies, then the default plan.
    env.clock.set(T0);
    let t = env
        .ent
        .start_trial(
            &admin(),
            TrialInput {
                subject: pk("bob"),
                plan: "pro".into(),
                days: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(t.status, SubscriptionStatus::Trialing);
    assert_eq!(t.trial_ends_at, Some(T0 + 14 * DAY));
    let g = env.ent.entitlements_of(&pk("bob"), None).features["ai"].clone();
    assert_eq!((g.limit, g.source), (Some(2), GrantSource::Trial));
    env.clock.set(T0 + 14 * DAY);
    let e = env.ent.entitlements_of(&pk("bob"), None);
    assert_eq!(e.plan.as_deref(), Some("free"));
    assert!(!e.features.contains_key("ai"));
    let report = env.ent.run_maintenance().await.unwrap();
    assert_eq!(report.trials_expired, 1);
    assert_eq!(
        env.ent.subscription(&admin(), &pk("bob")).unwrap().unwrap().status,
        SubscriptionStatus::Expired
    );
}

#[tokio::test]
async fn leases_cap_concurrency_and_gauges_cap_levels() {
    let env = Env::new().await;
    env.feature("runners", FeatureKind::Concurrency, &[]).await;
    env.feature("storage", FeatureKind::Gauge, &[]).await;
    env.plan("p", vec![pf("runners", Some(2), None), pf("storage", Some(100), None)]).await;
    let s = pk("alice");
    env.subscribe(&s, "p").await;
    let a = env.ent.acquire_lease(&sys(), req("runners", 1, "a").for_subject(&s).ttl(30)).await.unwrap();
    env.ent.acquire_lease(&sys(), req("runners", 1, "b").for_subject(&s).ttl(300)).await.unwrap();
    assert!(exceeded(&env.ent.acquire_lease(&sys(), req("runners", 1, "c").for_subject(&s)).await));
    env.ent.release_lease(&sys(), &a.id).await.unwrap();
    let c = env.ent.acquire_lease(&sys(), req("runners", 1, "c2").for_subject(&s).ttl(30)).await.unwrap();
    assert!(exceeded(&env.ent.acquire_lease(&sys(), req("runners", 1, "d").for_subject(&s)).await));
    env.clock.advance(31_000); // c expires
    env.ent.acquire_lease(&sys(), req("runners", 1, "d2").for_subject(&s)).await.unwrap();
    let r = env.ent.renew_lease(&sys(), &c.id, None).await;
    assert!(matches!(r, Err(EntitlementError::Conflict { code: "lease_expired", .. })));
    assert_eq!(env.ent.run_maintenance().await.unwrap().leases_expired, 1);

    let g = |delta: Option<i64>, value: Option<i64>, key: &str| GaugeRequest {
        feature: "storage".into(),
        delta,
        value,
        subject: Some(s.clone()),
        idempotency_key: Some(key.into()),
        ..GaugeRequest::default()
    };
    env.ent.adjust_gauge(&sys(), g(Some(60), None, "1")).await.unwrap();
    assert!(exceeded(&env.ent.adjust_gauge(&sys(), g(Some(50), None, "2")).await));
    let c = env.ent.adjust_gauge(&sys(), g(Some(-30), None, "3")).await.unwrap();
    assert_eq!(c.decision.used, 30);
    env.ent.adjust_gauge(&sys(), g(None, Some(100), "4")).await.unwrap();
    assert!(env.ent.adjust_gauge(&sys(), g(Some(-101), None, "5")).await.is_err());
    // A gauge re-arms its alerts when the level drops.
    env.ent.adjust_gauge(&sys(), g(None, Some(10), "6")).await.unwrap();
    env.ent.adjust_gauge(&sys(), g(None, Some(95), "7")).await.unwrap();
    let t: Vec<i64> = env.ent.alerts(&sys(), Some(&s)).await.unwrap().iter().map(|a| a.threshold).collect();
    assert_eq!(t.iter().filter(|x| **x == 90).count(), 2, "{t:?}");
}

#[tokio::test]
async fn billing_sync_is_idempotent_and_ordered() {
    let env = Env::new().await;
    env.feature("ai", FeatureKind::Metered, &[]).await;
    env.plan("free", vec![]).await;
    env.plan("pro", vec![pf("ai", Some(5), Some("month"))]).await;
    let s = "org:acme".to_owned();
    let sync = |plan: &str, id: &str, at: i64| forgebase_entitlements::SubscriptionSync {
        subject: s.clone(),
        plan: plan.into(),
        status: SubscriptionStatus::Active,
        trial_ends_at: None,
        current_period_start: None,
        current_period_end: None,
        cycle_anchor: None,
        cancel_at: None,
        external_ref: Some("sub_123".into()),
        addons: None,
        event_id: Some(id.into()),
        event_at: Some(at),
    };
    use forgebase_entitlements::BillingSync;
    let a = env.ent.sync_subscription(sync("pro", "evt_2", T0)).await.unwrap();
    assert_eq!(a.result, "applied");
    let b = env.ent.sync_subscription(sync("pro", "evt_2", T0)).await.unwrap();
    assert_eq!(b.result, "replayed");
    let c = env.ent.sync_subscription(sync("free", "evt_1", T0 - MIN)).await.unwrap();
    assert_eq!((c.result.as_str(), c.plan.as_str()), ("stale", "pro"));
    assert_eq!(env.ent.entitlements_of(&s, None).plan.as_deref(), Some("pro"));
    let mut cancel = sync("pro", "evt_3", T0 + MIN);
    cancel.status = SubscriptionStatus::Canceled;
    env.ent.sync_subscription(cancel).await.unwrap();
    assert!(!env.ent.entitlements_of(&s, None).entitled);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_entitlement_audit WHERE action = 'subscription_sync'").await,
        3
    );
    // The audit log is append-only.
    let mut tx = forgebase_db::Backend::begin(&env.db, forgebase_db::TxMode::Normal).await.unwrap();
    let e = tx
        .execute(&forgebase_db::Statement::new("DELETE FROM _forgebase_entitlement_audit", vec![]))
        .await;
    assert!(e.is_err());
    drop(tx);
}

#[tokio::test]
async fn usage_events_are_immutable() {
    let env = Env::new().await;
    env.feature("ai", FeatureKind::Metered, &[]).await;
    env.plan("p", vec![pf("ai", Some(5), Some("month"))]).await;
    env.subscribe(&pk("alice"), "p").await;
    env.ent.consume(&sys(), req("ai", 1, "k").for_subject(pk("alice"))).await.unwrap();
    for sql in [
        "UPDATE _forgebase_usage_events SET units = 0",
        "DELETE FROM _forgebase_usage_events",
    ] {
        let mut tx = forgebase_db::Backend::begin(&env.db, forgebase_db::TxMode::Normal).await.unwrap();
        let e = tx.execute(&forgebase_db::Statement::new(sql, vec![])).await;
        assert!(e.is_err(), "{sql}");
    }
    let page = env.ent.events(&admin(), None, None, None, None).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].units, 1);
}

#[tokio::test]
async fn data_core_record_calls_run_both_hooks() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    env.member("alice", &org, "bob", &["member"]).await;
    let p = env
        .policy
        .create_project(&user("alice").with_org(&org), Some(&org), "app", "app")
        .await
        .unwrap();
    let tenant = p["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "production")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    env.data
        .create_collection(
            &sys(),
            serde_json::from_value(serde_json::json!({
                "name": "notes", "options": {"tenant_scoped": true}, "fields": [{"name": "t", "type": "text"}]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    env.feature("notes", FeatureKind::Boolean, &["data:notes:create"]).await;
    env.feature("records", FeatureKind::Metered, &[]).await;
    env.policy_for(
        "records",
        QuotaPolicy {
            permissions: vec!["data:notes:create".into()],
            ..QuotaPolicy::default()
        },
    )
    .await;
    let mut off = pf("notes", None, None);
    off.enabled = false;
    env.plan("free", vec![off, pf("records", Some(1), Some("month"))]).await;
    env.subscribe(&org_subject(&org), "free").await;
    let bob = user("bob").with_tenant(&tenant);
    let r = env.data.create_record(&bob, "notes", serde_json::json!({"t": "a"})).await;
    assert!(r.is_err(), "entitlement step denies the record call");
    let reads = env.data.list_records(&bob, "notes", &Default::default()).await;
    assert!(reads.is_ok(), "only the gated permission is denied: {reads:?}");
    env.plan("free", vec![pf("notes", None, None), pf("records", Some(1), Some("month"))]).await;
    env.data.create_record(&bob, "notes", serde_json::json!({"t": "a"})).await.unwrap();
    // The operation meters itself; once the quota is used, step 6 denies.
    env.ent
        .consume(&sys(), req("records", 1, "note-1").for_subject(org_subject(&org)))
        .await
        .unwrap();
    let r = env.data.create_record(&bob, "notes", serde_json::json!({"t": "b"})).await;
    assert!(r.is_err(), "quota step denies");
}
