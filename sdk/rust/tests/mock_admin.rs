//! Mock tests for customization, instance administration, billing admin and entitlements admin.
//! Hermetic: no ForgeBase binary needed.

mod common;

use std::time::Duration;

use common::mock::{Mock, err, no_content, ok, serve, status, text};
use common::sample::{schema, with};
use forgebase_sdk::billing_admin::{ListAllOrders, ListInvoices, StatsOptions, SubjectListOptions, UsageMetersOptions};
use forgebase_sdk::customize::{BrandingAsset, EmailDeliveriesQuery, MailProviderConfig};
use forgebase_sdk::entitlements_admin::UsageEventsOptions;
use forgebase_sdk::generated::{
    AcceptItem, AgreementInput, BillingSettings, BrandingInput, CancelInput, ChangePlanInput, CouponInput,
    CreateSnapshot, CustomLimit, CustomerInput, DeliveryStatus, GaugeRequest, LoginSpec, OrderInput,
    PaymentMethodInput, PreviewInput, PriceInput, ProductInput, ProviderInput, RebalancePlanBody, Retention, SendInput,
    SubscribeInput, TaxRateInput, TemplateInput, UpdateShard,
};
use forgebase_sdk::system::PlacementsOptions;
use serde_json::{Value, json};

// ---- customize ----

#[tokio::test]
async fn customize_domains_branding_and_hosted_views() {
    let dom = with("Domain", json!({"id": "d1", "host": "auth.acme.test"}));
    let m = Mock::start(vec![
        ok(json!({"items": [dom.clone()]})),
        status(201, dom.clone()),
        ok(dom.clone()),
        no_content(),
        ok(with(
            "VerifyResult",
            json!({"verified": false, "reason": "txt record missing"}),
        )),
        ok(dom),
        ok(schema("Branding")),
        ok(schema("Branding")),
        ok(schema("Branding")),
        ok(schema("Branding")),
        ok(schema("PublicBranding")),
        ok(json!({"items": [schema("Agreement")]})),
        text("image/png", "PNGDATA"),
        text("text/css", ":root{--fb-primary:#123}"),
        text("text/html", "<html></html>"),
    ])
    .await;
    let c = m.client().customize();
    let d = c.domains();
    assert_eq!(d.list(Some("eu")).await.expect("list")[0].host, "auth.acme.test");
    assert_eq!(m.last().q("realm").as_deref(), Some("eu"));
    d.add("auth.acme.test", None).await.expect("add");
    assert_eq!(m.last().json(), json!({"host": "auth.acme.test"}));
    d.get("d1").await.expect("get");
    d.delete("d1").await.expect("delete");
    let v = d.verify("d1").await.expect("verify");
    assert!(!v.verified);
    assert_eq!(m.last().path, "/api/v1/customize/domains/d1/verify");
    d.reissue("d1").await.expect("reissue");

    let b = c.branding();
    b.get(None).await.expect("branding");
    b.put(&BrandingInput::default(), Some("eu")).await.expect("put");
    assert_eq!(
        (m.last().method.as_str(), m.last().q("realm").as_deref()),
        ("PUT", Some("eu"))
    );
    b.upload_asset(BrandingAsset::Logo, &b"\x89PNG"[..], "image/png", None)
        .await
        .expect("upload");
    let last = m.last();
    assert_eq!(
        (last.method.as_str(), last.route()),
        ("PUT", "/api/v1/customize/branding/assets/logo")
    );
    assert_eq!(last.header("content-type"), Some("image/png"));
    b.delete_asset(BrandingAsset::Favicon, None)
        .await
        .expect("delete asset");
    assert_eq!(m.last().path, "/api/v1/customize/branding/assets/favicon");

    let h = c.hosted();
    h.branding(Some("eu")).await.expect("public branding");
    assert!(
        m.last().header("x-api-key").is_none(),
        "hosted views send no credentials"
    );
    assert_eq!(h.agreements(None).await.expect("agreements").len(), 1);
    assert_eq!(
        &h.asset(BrandingAsset::Logo, None).await.expect("asset")[..],
        b"PNGDATA"
    );
    assert!(
        h.asset_url(BrandingAsset::Logo, Some("eu"))
            .ends_with("/api/v1/customize/public/branding/logo?realm=eu")
    );
    assert!(h.theme_url(None).ends_with("/api/v1/customize/public/theme.css"));
    assert!(h.theme(None).await.expect("theme").contains("--fb-primary"));
    assert!(c.docs_url(None).ends_with("/api/v1/customize/docs"));
    assert_eq!(c.docs(None).await.expect("docs"), "<html></html>");
}

#[tokio::test]
async fn customize_email_templates_provider_and_test_send() {
    let m = Mock::start(vec![
        ok(json!({"items": [schema("EmailTemplate")]})),
        ok(schema("EmailTemplate")),
        ok(schema("EmailTemplate")),
        no_content(),
        ok(schema("Preview")),
        status(202, schema("Delivery")),
        ok(json!({"items": [schema("Delivery")]})),
        ok(schema("Delivery")),
        status(202, schema("Delivery")),
        ok(json!({"realm": "eu", "configured": true, "config": {"provider": "resend", "api_key": "********"}, "created_at": 1794619038000_i64, "updated_at": 1794619038000_i64})),
        ok(json!({"realm": "eu", "configured": true, "config": {"provider": "postmark"}})),
        no_content(),
        ok(json!({"provider": "postmark", "message_id": "m-1", "from": "noreply@acme.test"})),
        err(409, "mail_test_failed"),
    ])
    .await;
    let e = m.client().customize().email();
    e.templates(None).await.expect("templates");
    e.template("magic_link", None).await.expect("template");
    e.put_template("welcome", &TemplateInput::default(), None)
        .await
        .expect("put");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("PUT", "/api/v1/customize/email/templates/welcome")
    );
    e.delete_template("welcome", None).await.expect("delete");
    e.preview(
        &PreviewInput {
            template: Some("welcome".into()),
            variables: Some(json!({"name": "Ada"})),
            ..PreviewInput::default()
        },
        None,
    )
    .await
    .expect("preview");
    assert_eq!(
        m.last().json(),
        json!({"template": "welcome", "variables": {"name": "Ada"}})
    );
    e.send(
        &SendInput {
            template: "welcome".into(),
            to: "a@b.c".into(),
            variables: None,
        },
        None,
    )
    .await
    .expect("send");
    let q = EmailDeliveriesQuery {
        status: Some(DeliveryStatus::Failed),
        to: Some("a@b.c".into()),
        limit: Some(5),
        before: Some(99),
        ..EmailDeliveriesQuery::default()
    };
    e.deliveries(&q).await.expect("deliveries");
    let seen = m.last();
    assert_eq!(
        (
            seen.q("status").as_deref(),
            seen.q("to").as_deref(),
            seen.q("limit").as_deref(),
            seen.q("before").as_deref()
        ),
        (Some("failed"), Some("a@b.c"), Some("5"), Some("99"))
    );
    e.delivery("x1").await.expect("delivery");
    e.retry("x1").await.expect("retry");
    assert_eq!(m.last().path, "/api/v1/customize/email/deliveries/x1/retry");

    let st = e.provider(Some("eu")).await.expect("provider");
    assert!(st.configured);
    assert_eq!(st.config["api_key"], "********", "secrets are masked");
    let cfg = MailProviderConfig::new("postmark")
        .from_address("noreply@acme.test")
        .set("server_token", "tok")
        .set("message_stream", "outbound");
    e.put_provider(&cfg, Some("eu")).await.expect("put provider");
    assert_eq!(
        m.last().json(),
        json!({"provider": "postmark", "from_address": "noreply@acme.test", "server_token": "tok", "message_stream": "outbound"})
    );
    assert_eq!(m.last().q("realm").as_deref(), Some("eu"));
    e.delete_provider(Some("*")).await.expect("delete provider");
    assert_eq!(m.last().q("realm").as_deref(), Some("*"));
    let t = e.test_provider("me@acme.test", None).await.expect("test send");
    assert_eq!(
        (t.provider.as_str(), t.message_id.as_deref()),
        ("postmark", Some("m-1"))
    );
    assert_eq!(m.last().json(), json!({"to": "me@acme.test"}));
    let failed = e.test_provider("me@acme.test", None).await.expect_err("409");
    assert_eq!((failed.status(), failed.code()), (Some(409), "mail_test_failed"));
}

#[tokio::test]
async fn customize_agreements() {
    let m = Mock::start(vec![
        ok(json!({"items": [schema("Agreement")]})),
        status(201, schema("Agreement")),
        ok(json!({"pending": [schema("AgreementRef")]})),
        ok(json!({"items": [schema("Acceptance")]})),
        ok(json!({"items": []})),
        ok(json!({"items": [schema("Acceptance")]})),
    ])
    .await;
    let a = m.client().customize().agreements();
    a.list(None, Some("terms")).await.expect("list");
    assert_eq!(m.last().q("kind").as_deref(), Some("terms"));
    a.publish(
        &AgreementInput {
            kind: "terms".into(),
            title: "Terms".into(),
            body: "…".into(),
            url: None,
        },
        None,
    )
    .await
    .expect("publish");
    assert_eq!(a.pending().await.expect("pending").len(), 1);
    a.accept(vec![AcceptItem {
        kind: "terms".into(),
        version: 3,
    }])
    .await
    .expect("accept");
    assert_eq!(m.last().json(), json!({"items": [{"kind": "terms", "version": 3}]}));
    assert!(a.accepted().await.expect("accepted").is_empty());
    a.acceptances(None, Some("user:users/u1"), None, Some(10))
        .await
        .expect("acceptances");
    assert_eq!(m.last().q("principal").as_deref(), Some("user:users/u1"));
}

// ---- system ----

#[tokio::test]
async fn system_info_capabilities_features_and_config() {
    let m = Mock::start(vec![
        ok(json!({"name": "forgebase", "database": {"capabilities": []}})),
        ok(with(
            "Capabilities",
            json!({"backend": "sqlite", "features": [with("Capability", json!({"feature": "opencypher", "engine": "portable", "accelerated": false, "state": "portable"}))]}),
        )),
        ok(schema("SystemFeatures")),
        ok(schema("SystemConfigPatched")),
        ok(json!({"items": [schema("ExtensionInfo")]})),
        status(201, schema("ExtensionInfo")),
        no_content(),
    ])
    .await;
    let s = m.client().system();
    assert_eq!(s.info().await.expect("info")["name"], "forgebase");
    let caps = s.capabilities().await.expect("capabilities");
    assert_eq!(caps.backend, "sqlite");
    assert_eq!(caps.features.len(), 1);
    assert_eq!(m.last().route(), "/api/system/capabilities");
    s.features().await.expect("features");
    s.update_config(&json!({"server.port": 8091})).await.expect("config");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PATCH", json!({"set": {"server.port": 8091}}))
    );
    assert_eq!(s.extensions().list().await.expect("extensions").len(), 1);
    s.extensions().install("pg_trgm").await.expect("install");
    assert_eq!(m.last().json(), json!({"name": "pg_trgm"}));
    s.extensions().drop_extension("pg_trgm").await.expect("drop");
}

#[tokio::test]
async fn system_backups_and_db_logins() {
    let snap = schema("SnapshotView");
    let m = Mock::start(vec![
        ok(json!({"items": [snap.clone()]})),
        status(201, schema("Manifest")),
        ok(snap),
        ok(with("VerifyReport", json!({"ok": true}))),
        ok(json!({"deleted": ["old1", "old2"]})),
        no_content(),
        ok(json!({"items": [schema("DbLogin")]})),
        status(201, schema("LoginSecret")),
        ok(schema("DbLogin")),
        ok(schema("LoginSecret")),
        no_content(),
    ])
    .await;
    let s = m.client().system();
    let b = s.backups();
    assert_eq!(b.list().await.expect("list").len(), 1);
    b.create(&CreateSnapshot {
        label: Some("nightly".into()),
        include_indexes: Some(true),
        retention: Some(Retention {
            keep_last: Some(7),
            max_age_days: Some(30),
        }),
    })
    .await
    .expect("create");
    assert_eq!(
        m.last().json(),
        json!({"label": "nightly", "include_indexes": true, "retention": {"keep_last": 7, "max_age_days": 30}})
    );
    b.get("latest").await.expect("get");
    assert!(b.verify("latest").await.expect("verify").ok);
    assert_eq!(m.last().path, "/api/v1/system/backups/latest/verify");
    assert_eq!(
        b.prune(&Retention {
            keep_last: Some(3),
            max_age_days: None
        })
        .await
        .expect("prune"),
        ["old1", "old2"]
    );
    assert_eq!(m.last().json(), json!({"keep_last": 3}));
    b.delete("old1").await.expect("delete");

    let l = s.db_logins();
    assert_eq!(l.list().await.expect("logins").len(), 1);
    l.create(&LoginSpec {
        name: "analyst".into(),
        readonly: Some(true),
        admin: None,
        connection_limit: None,
        principal: None,
        statement_timeout_ms: None,
        tenant: None,
        valid_until: None,
    })
    .await
    .expect("create login");
    assert_eq!(m.last().json(), json!({"name": "analyst", "readonly": true}));
    l.get("analyst").await.expect("get");
    l.rotate("analyst", Some("2027-01-01T00:00:00Z")).await.expect("rotate");
    assert_eq!(m.last().json(), json!({"valid_until": "2027-01-01T00:00:00Z"}));
    l.delete("analyst").await.expect("delete");
}

#[tokio::test]
async fn system_database_topology_and_operations() {
    let op = |status: &str| with("Operation", json!({"id": "op1", "status": status, "kind": "promote"}));
    let m = Mock::start(vec![
        ok(schema("TopologyView")),
        ok(schema("NodeDetail")),
        status(202, json!({"operation": op("running")})),
        status(202, json!({"operation": op("running")})),
        ok(schema("PromoteCheck")),
        ok(json!({"removed": "replica-1"})),
        ok(with("PlacementPage", json!({"next_cursor": "c2"}))),
        status(202, json!({"operation": op("running")})),
        ok(schema("RebalancePlanView")),
        ok(json!({"items": [op("succeeded")]})),
        ok(op("running")),
        ok(op("running")),
        ok(op("succeeded")),
        ok(schema("SetInfo")),
    ])
    .await;
    let d = m.client().system().database();
    d.topology().await.expect("topology");
    d.node("n1").await.expect("node");
    let o = d.promote("n1", "replica-1", true).await.expect("promote");
    assert_eq!(o.id, "op1");
    assert_eq!(
        m.last().json(),
        json!({"confirm": "replica-1", "accept_data_loss": true})
    );
    d.rebuild_node("n1").await.expect("rebuild");
    assert!(d.promote_check("n1").await.is_ok());
    assert_eq!(d.remove_node("n1", "replica-1").await.expect("remove"), "replica-1");
    assert_eq!(m.last().q("confirm").as_deref(), Some("replica-1"));
    let page = d
        .placements(&PlacementsOptions {
            set: Some("control".into()),
            search: Some("acme".into()),
            limit: Some(20),
            ..PlacementsOptions::default()
        })
        .await
        .expect("placements");
    assert_eq!(page.next_cursor.as_deref(), Some("c2"));
    let q = m.last();
    assert_eq!(
        (q.q("set").as_deref(), q.q("q").as_deref(), q.q("limit").as_deref()),
        (Some("control"), Some("acme"), Some("20"))
    );
    d.move_tenant("t 1", "set2", "t 1").await.expect("move");
    assert_eq!(m.last().path, "/api/system/database/placements/t%201/move");
    d.rebalance_plan(&RebalancePlanBody {
        max_moves: Some(3),
        sets: None,
    })
    .await
    .expect("plan");
    assert_eq!(d.operations(true, Some(5)).await.expect("operations").len(), 1);
    assert_eq!(m.last().q("active").as_deref(), Some("true"));
    d.operation("op1").await.expect("operation");
    let done = d
        .wait_for_operation("op1", Duration::from_millis(5), Some(Duration::from_secs(5)))
        .await
        .expect("wait");
    assert_eq!(done.status, "succeeded");
    d.update_shard(
        "s1",
        &UpdateShard {
            state: Some("draining".into()),
            ..UpdateShard::default()
        },
    )
    .await
    .expect("shard");
    assert_eq!(
        (m.last().method.as_str(), m.last().json()),
        ("PATCH", json!({"state": "draining"}))
    );
}

#[tokio::test]
async fn system_wait_for_operation_times_out() {
    let m = Mock::start(vec![ok(with("Operation", json!({"id": "op1", "status": "running"})))]).await;
    let e = m
        .client()
        .system()
        .database()
        .wait_for_operation("op1", Duration::from_millis(5), Some(Duration::from_millis(30)))
        .await
        .expect_err("still running");
    assert_eq!(e.code(), "config_error");
}

// ---- billing admin ----

#[tokio::test]
async fn billing_customers_portal_subscriptions_orders() {
    let checkout = schema("Checkout");
    let m = Mock::start(vec![
        ok(schema("Customer")),
        ok(schema("Customer")),
        ok(json!({"items": [schema("Customer")]})),
        ok(schema("Portal")),
        ok(schema("HostedPage")),
        ok(json!({"items": [schema("BillingSubscription")]})),
        ok(schema("BillingSubscription")),
        status(201, checkout.clone()),
        ok(checkout.clone()),
        ok(schema("ProrationPreview")),
        ok(checkout.clone()),
        ok(schema("BillingSubscription")),
        ok(checkout.clone()),
        ok(json!({"items": [schema("Order")]})),
        ok(schema("Order")),
        status(201, checkout),
        ok(with("OrderPage", json!({"total": 1}))),
    ])
    .await;
    let b = m.client().billing();
    b.customers().get(Some("org:1")).await.expect("customer");
    assert_eq!(m.last().q("subject").as_deref(), Some("org:1"));
    b.customers()
        .put(&CustomerInput::default())
        .await
        .expect("put customer");
    assert_eq!(m.last().method, "PUT");
    assert_eq!(b.customers().list(Some(5)).await.expect("customers").len(), 1);
    b.portal().get(None).await.expect("portal");
    b.portal()
        .payment_method(&PaymentMethodInput::default())
        .await
        .expect("hosted page");
    assert_eq!(m.last().path, "/api/v1/billing/portal/payment-method");

    let s = b.subscriptions();
    assert_eq!(s.list(None).await.expect("list").len(), 1);
    s.get("sub 1").await.expect("get");
    assert_eq!(m.last().path, "/api/v1/billing/subscriptions/sub%201");
    s.create(&SubscribeInput::default()).await.expect("subscribe");
    assert_eq!(m.last().route(), "/api/v1/billing/subscriptions");
    s.change("sub1", &ChangePlanInput::default()).await.expect("change");
    s.preview_change("sub1", &ChangePlanInput::default())
        .await
        .expect("preview");
    assert_eq!(m.last().path, "/api/v1/billing/subscriptions/sub1/change/preview");
    s.cancel(
        "sub1",
        &CancelInput {
            at_period_end: Some(false),
            ..CancelInput::default()
        },
    )
    .await
    .expect("cancel");
    assert_eq!(m.last().json(), json!({"at_period_end": false}));
    s.pause("sub1").await.expect("pause");
    s.resume("sub1").await.expect("resume");
    assert_eq!(m.last().path, "/api/v1/billing/subscriptions/sub1/resume");

    let o = b.orders();
    assert_eq!(o.list(None).await.expect("orders").len(), 1);
    o.get("o1").await.expect("get order");
    o.create(&OrderInput::default()).await.expect("order");
    assert_eq!(m.last().route(), "/api/v1/billing/orders");
    let page = o
        .list_all(&ListAllOrders {
            status: Some("paid".into()),
            limit: Some(10),
            offset: Some(20),
            ..ListAllOrders::default()
        })
        .await
        .expect("all orders");
    assert_eq!(page.total, 1);
    assert_eq!(
        (m.last().route(), m.last().q("offset").as_deref()),
        ("/api/v1/billing/admin/orders", Some("20"))
    );
}

#[tokio::test]
async fn billing_invoices_catalog_providers_settings_and_ops() {
    let m = Mock::start(vec![
        ok(json!({"items": [schema("Invoice")]})),
        ok(schema("Invoice")),
        ok(schema("BillingCollection")),
        ok(schema("Invoice")),
        ok(schema("Invoice")),
        status(201, schema("CreditNote")),
        ok(json!({"items": [schema("CreditNote")]})),
        ok(json!({"items": [schema("Payment")]})),
        ok(json!({"items": [schema("Refund")]})),
        ok(json!({"items": [schema("Product")]})),
        ok(schema("Product")),
        ok(schema("Product")),
        ok(json!({"items": [schema("Price")]})),
        ok(schema("Price")),
        ok(schema("Price")),
        ok(json!({"items": [schema("Coupon")]})),
        ok(schema("Coupon")),
        ok(schema("Coupon")),
        ok(json!({"items": [schema("TaxRate")]})),
        ok(schema("TaxRate")),
        ok(schema("TaxRate")),
        ok(json!({"items": [schema("ProviderAccount")]})),
        ok(schema("ProviderAccount")),
        no_content(),
        ok(schema("BillingSettings")),
        ok(schema("BillingSettings")),
        ok(schema("BillingStats")),
        ok(schema("UsageMeterPage")),
        ok(json!({"items": [schema("DunningEntry")]})),
        ok(schema("BillingMaintenanceReport")),
    ])
    .await;
    let b = m.client().billing();
    let inv = b.invoices();
    inv.list(&ListInvoices {
        list: SubjectListOptions {
            subject: Some("org:1".into()),
            limit: Some(10),
        },
        status: Some("open".into()),
    })
    .await
    .expect("invoices");
    let q = m.last();
    assert_eq!(
        (
            q.q("subject").as_deref(),
            q.q("status").as_deref(),
            q.q("limit").as_deref()
        ),
        (Some("org:1"), Some("open"), Some("10"))
    );
    inv.get("i1").await.expect("get");
    inv.pay("i1", Some("https://app/back")).await.expect("pay");
    assert_eq!(m.last().json(), json!({"return_url": "https://app/back"}));
    inv.void("i1").await.expect("void");
    inv.mark_paid("i1", Some("wire-77")).await.expect("mark paid");
    assert_eq!(m.last().json(), json!({"reference": "wire-77"}));
    inv.credit_note("i1", &schema_input("CreditNoteInput"))
        .await
        .expect("credit note");
    assert_eq!(m.last().path, "/api/v1/billing/invoices/i1/credit-notes");
    assert_eq!(
        b.credit_notes()
            .list(&SubjectListOptions::default())
            .await
            .expect("credit notes")
            .len(),
        1
    );
    assert_eq!(
        b.payments()
            .list(&SubjectListOptions::default())
            .await
            .expect("payments")
            .len(),
        1
    );
    assert_eq!(
        b.refunds()
            .list(&SubjectListOptions::default())
            .await
            .expect("refunds")
            .len(),
        1
    );
    assert_eq!(m.last().route(), "/api/v1/billing/refunds");

    let cat = b.catalog();
    assert_eq!(cat.products().list().await.expect("products").len(), 1);
    cat.products()
        .put("pro", &schema_input::<ProductInput>("ProductInput"))
        .await
        .expect("put product");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("PUT", "/api/v1/billing/products/pro")
    );
    cat.products().archive("pro").await.expect("archive product");
    assert_eq!(cat.prices().list(Some("pro")).await.expect("prices").len(), 1);
    assert_eq!(m.last().q("product").as_deref(), Some("pro"));
    cat.prices()
        .put("pro_monthly", &schema_input::<PriceInput>("PriceInput"))
        .await
        .expect("put price");
    cat.prices().archive("pro_monthly").await.expect("archive price");
    assert_eq!(cat.coupons().list().await.expect("coupons").len(), 1);
    cat.coupons()
        .put("SAVE10", &schema_input::<CouponInput>("CouponInput"))
        .await
        .expect("put coupon");
    cat.coupons().archive("SAVE10").await.expect("archive coupon");
    assert_eq!(cat.tax_rates().list().await.expect("tax rates").len(), 1);
    cat.tax_rates()
        .put("US-CA", &schema_input::<TaxRateInput>("TaxRateInput"))
        .await
        .expect("put tax");
    cat.tax_rates().archive("US-CA").await.expect("archive tax");
    assert_eq!(m.last().path, "/api/v1/billing/tax-rates/US-CA/archive");

    assert_eq!(b.providers().list().await.expect("providers").len(), 1);
    b.providers()
        .put("stripe", &schema_input::<ProviderInput>("ProviderInput"))
        .await
        .expect("put provider");
    b.providers().delete("stripe").await.expect("delete provider");
    let settings = b.settings().get().await.expect("settings");
    b.settings().put(&settings).await.expect("put settings");
    assert_eq!(m.last().method, "PUT");

    b.stats(&StatsOptions {
        interval: Some("month".into()),
        ..StatsOptions::default()
    })
    .await
    .expect("stats");
    assert_eq!(m.last().q("interval").as_deref(), Some("month"));
    b.usage_meters(&UsageMetersOptions {
        min_percent: Some(90),
        feature: Some("api".into()),
        ..UsageMetersOptions::default()
    })
    .await
    .expect("meters");
    assert_eq!(m.last().q("min_percent").as_deref(), Some("90"));
    assert_eq!(b.dunning(Some(20)).await.expect("dunning").len(), 1);
    b.maintenance().await.expect("maintenance");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("POST", "/api/v1/billing/maintenance")
    );
}

/// A request model built from its schema's required fields.
fn schema_input<T: serde::de::DeserializeOwned>(name: &str) -> T {
    serde_json::from_value(schema(name)).unwrap_or_else(|e| panic!("{name} does not decode from its own schema: {e}"))
}

#[test]
fn billing_settings_round_trips_unchanged() {
    // `settings().put` takes what `get` returned: the model must keep every field it was given.
    let v = schema("BillingSettings");
    let s: BillingSettings = serde_json::from_value(v.clone()).expect("decode");
    let back: Value = serde_json::to_value(&s).expect("encode");
    for (k, val) in v.as_object().expect("object") {
        assert_eq!(&back[k], val, "{k}");
    }
}

// ---- entitlements admin ----

#[tokio::test]
async fn entitlements_catalog_overrides_and_limits() {
    let m = Mock::start(vec![
        ok(json!({"items": [schema("Plan")]})),
        ok(schema("Plan")),
        ok(schema("Plan")),
        no_content(),
        ok(json!({"items": [schema("Feature")]})),
        ok(schema("Feature")),
        ok(schema("Feature")),
        no_content(),
        ok(json!({"items": [schema("Addon")]})),
        ok(schema("Addon")),
        no_content(),
        ok(json!({"items": [schema("UsagePack")]})),
        ok(schema("UsagePack")),
        no_content(),
        ok(json!({"items": [schema("PackGrant")]})),
        status(201, schema("PackGrant")),
        ok(json!({"items": [schema("QuotaPolicy")]})),
        ok(schema("QuotaPolicy")),
        no_content(),
        ok(json!({"items": [schema("Override")]})),
        status(201, schema("Override")),
        no_content(),
        ok(json!({"items": [schema("CustomLimit")]})),
        ok(schema("CustomLimit")),
        no_content(),
    ])
    .await;
    let e = m.client().entitlements();
    let plans = e.plans();
    assert_eq!(plans.list().await.expect("plans").len(), 1);
    plans.get("pro").await.expect("get");
    plans.put("pro", &schema_input("PlanInput")).await.expect("put");
    assert_eq!(
        (m.last().method.as_str(), m.last().path.as_str()),
        ("PUT", "/api/v1/plans/pro")
    );
    plans.delete("pro").await.expect("delete");
    let f = e.features();
    assert_eq!(f.list().await.expect("features").len(), 1);
    f.get("ai").await.expect("get");
    f.put("ai", &schema_input("FeatureInput")).await.expect("put");
    f.delete("ai").await.expect("delete");
    let a = e.addons();
    assert_eq!(a.list().await.expect("addons").len(), 1);
    a.put("seats", &schema_input("AddonInput")).await.expect("put");
    a.delete("seats").await.expect("delete");
    let p = e.usage_packs();
    assert_eq!(p.list().await.expect("packs").len(), 1);
    p.put("boost", &schema_input("UsagePackInput")).await.expect("put");
    p.delete("boost").await.expect("delete");
    assert_eq!(p.grants(Some("org:1")).await.expect("grants").len(), 1);
    assert_eq!(m.last().q("subject").as_deref(), Some("org:1"));
    p.grant(&schema_input("PackGrantInput")).await.expect("grant");
    assert_eq!(m.last().route(), "/api/v1/entitlements/usage-packs");
    let q = e.quota_policies();
    assert_eq!(q.list().await.expect("policies").len(), 1);
    q.put("ai", &schema_input("QuotaPolicy")).await.expect("put");
    q.delete("ai").await.expect("delete");
    let o = e.overrides();
    assert_eq!(o.list(None).await.expect("overrides").len(), 1);
    o.add(&schema_input("OverrideInput")).await.expect("add");
    o.delete("ov1").await.expect("delete");
    let l = e.limits();
    assert_eq!(l.list(Some("org:1")).await.expect("limits").len(), 1);
    l.put(&schema_input::<CustomLimit>("CustomLimit")).await.expect("put");
    assert_eq!(m.last().method, "PUT");
    l.delete("org:1", "ai").await.expect("delete");
    let c = m.last();
    assert_eq!(
        (c.method.as_str(), c.q("subject").as_deref(), c.q("feature").as_deref()),
        ("DELETE", Some("org:1"), Some("ai"))
    );
}

#[tokio::test]
async fn entitlements_subscription_trials_gauge_events_alerts() {
    let m = Mock::start(vec![
        ok(schema("Subscription")),
        ok(schema("SyncOutcome")),
        status(201, schema("Subscription")),
        ok(schema("Subscription")),
        ok(schema("Consumption")),
        ok(with("UsageEventPage", json!({"next": 8}))),
        ok(json!({"items": [schema("UsageAlert")]})),
        ok(schema("EntitlementsMaintenanceReport")),
    ])
    .await;
    let e = m.client().entitlements();
    e.subscription(Some("org:1")).await.expect("subscription");
    e.sync_subscription(&schema_input("SubscriptionSync"))
        .await
        .expect("sync");
    assert_eq!(m.last().method, "PUT");
    e.start_trial(&schema_input("TrialInput")).await.expect("trial");
    e.set_subject_addon("org:1", "seats", 3).await.expect("addon");
    assert_eq!(
        m.last().json(),
        json!({"subject": "org:1", "addon": "seats", "quantity": 3})
    );
    let g: GaugeRequest = schema_input("GaugeRequest");
    e.gauge(&g).await.expect("gauge");
    assert_eq!(m.last().route(), "/api/v1/usage/gauge");
    let page = e
        .usage_events(&UsageEventsOptions {
            subject: Some("org:1".into()),
            feature: Some("ai".into()),
            after: Some(3),
            limit: Some(50),
        })
        .await
        .expect("events");
    assert_eq!(page.next, Some(8));
    assert_eq!(m.last().q("after").as_deref(), Some("3"));
    assert_eq!(e.usage_alerts(None).await.expect("alerts").len(), 1);
    e.maintenance().await.expect("maintenance");
}

#[tokio::test]
async fn admin_reads_are_forbidden_for_users() {
    let (_m, fb) = serve(err(403, "forbidden")).await;
    assert!(
        fb.billing()
            .customers()
            .list(None)
            .await
            .expect_err("403")
            .is_forbidden()
    );
    assert!(fb.entitlements().plans().list().await.expect_err("403").is_forbidden());
    assert!(fb.system().capabilities().await.expect_err("403").is_forbidden());
    let _ = (
        ChangePlanInput::default(),
        SendInput::default(),
        PreviewInput::default(),
    );
}
