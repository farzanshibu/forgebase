//! Live tests for the platform administration surface through the SDK against the real server
//! binary: identity (realms, providers, OAuth clients, service accounts and keys), customization
//! (branding, domains, templates, agreements, mail provider), billing and entitlements admin,
//! backups, indexes and documents. They check that the typed models decode what the server sends.
//!
//! Set `FB_LIVE_BIN`; without it these tests pass vacuously (see `tests/common`).

mod common;

use common::Server;
use forgebase_sdk::billing_admin::{ListInvoices, StatsOptions, SubjectListOptions, UsageMetersOptions};
use forgebase_sdk::customize::{BrandingAsset, MailProviderConfig};
use forgebase_sdk::documents::GetDocumentOptions;
use forgebase_sdk::generated::{AgreementInput, BrandingInput, CreateSnapshot, Retention};
use forgebase_sdk::identity::{
    ClientInput, NewApiKey, NewRealm, NewServiceAccount, ProviderPreset, RealmProviderConfig,
};
use forgebase_sdk::indexes::{IndexKey, IndexKind, NewIndex};
use forgebase_sdk::{ForgeBase, WriteOptions};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn identity_realm_provider_presets_oauth_clients_and_service_accounts() {
    let Some(s) = Server::start() else { return };
    let id = s.admin().await.identity();

    let realms = id.realms().list().await.expect("realms");
    assert!(realms.iter().any(|r| r.realm == "default"));
    let created = id.realms().create(&NewRealm::new("eu")).await.expect("create realm");
    assert_eq!(
        (created.realm.as_str(), created.collection.as_str()),
        ("eu", "eu_users")
    );
    assert_eq!(id.realms().get("eu").await.expect("get").collection, "eu_users");
    let mut cfg = created.config.clone();
    cfg.methods.magic_link = true;
    assert!(
        id.realms()
            .update("eu", None, &cfg)
            .await
            .expect("update")
            .config
            .methods
            .magic_link
    );
    id.realms().keys("eu").await.expect("keys");
    let pub_info = id.realms().info().await.expect("realm info");
    assert!(!pub_info.collection.is_empty());

    // Providers, including the ADR-0039 presets: secrets are sealed and come back masked.
    let providers = id.realms().providers();
    for (name, preset) in [
        ("gh", ProviderPreset::Github),
        ("bb", ProviderPreset::Bitbucket),
        ("dc", ProviderPreset::Discord),
    ] {
        let p = providers
            .put(
                "eu",
                name,
                &RealmProviderConfig::preset(preset, format!("{name}-client"), "super-secret"),
            )
            .await
            .unwrap_or_else(|e| panic!("put {name}: {e}"));
        assert_eq!(p.kind, preset.as_str());
        assert!(
            !format!("{:?}", p.extra).contains("super-secret"),
            "secrets never leave the server"
        );
    }
    let ms = RealmProviderConfig::preset(ProviderPreset::Microsoft, "ms-client", "ms-secret").tenant("organizations");
    assert_eq!(
        providers.put("eu", "entra", &ms).await.expect("microsoft preset").kind,
        "microsoft"
    );
    let gl = RealmProviderConfig::preset(ProviderPreset::Gitlab, "gl-client", "gl-secret")
        .base_url("https://gitlab.example.test");
    assert_eq!(
        providers.put("eu", "gitlab", &gl).await.expect("gitlab preset").kind,
        "gitlab"
    );
    let bad_tenant =
        RealmProviderConfig::preset(ProviderPreset::Microsoft, "id", "secret").tenant("contoso.onmicrosoft.com");
    assert!(
        providers
            .put("eu", "entra2", &bad_tenant)
            .await
            .expect_err("a tenant domain name is refused")
            .is_validation()
    );
    let oidc = RealmProviderConfig::preset(ProviderPreset::Oidc, "oidc-client", "oidc-secret")
        .set("issuer", "https://login.example.test");
    let r = providers.put("eu", "okta", &oidc).await;
    assert!(
        r.is_ok() || r.as_ref().err().is_some_and(|e| e.is_validation()),
        "{r:?}"
    );
    let listed = providers.list("eu").await.expect("list providers");
    assert!(listed.iter().any(|p| p.id == "bb" && p.kind == "bitbucket"));
    assert_eq!(providers.get("eu", "gh").await.expect("get").id, "gh");
    providers.delete("eu", "gh").await.expect("delete");
    assert!(providers.get("eu", "gh").await.expect_err("gone").is_not_found());
    assert!(id.realms().sms().get("eu").await.is_ok());

    // OAuth clients: the secret is shown once.
    let clients = id.clients();
    let made = clients
        .create(&ClientInput::new("Web app").redirect_uri("https://app.example.test/cb"))
        .await
        .expect("client");
    let secret = made.client_secret.clone().expect("a confidential client gets a secret");
    assert!(!secret.is_empty());
    assert_eq!(made.client.client_type, "confidential");
    assert!(
        clients
            .list(None)
            .await
            .expect("clients")
            .iter()
            .any(|c| c.client_id == made.client.client_id)
    );
    let off = clients
        .update(
            &made.client.client_id,
            &ClientInput {
                disabled: Some(true),
                ..ClientInput::default()
            },
        )
        .await
        .expect("disable");
    assert!(off.disabled);
    let rot = clients
        .rotate_secret(&made.client.client_id, Some(60))
        .await
        .expect("rotate");
    assert_ne!(rot.client_secret.as_deref(), Some(secret.as_str()));
    clients.delete(&made.client.client_id).await.expect("delete");
    let issuer = id.oidc().issuer("default");
    assert!(issuer.ends_with("/api/auth/oidc/default"));
    assert!(id.oidc().discovery("default").await.expect("discovery")["issuer"].is_string());
    assert!(id.oidc().jwks("default").await.expect("jwks")["keys"].is_array());

    // Service accounts and their keys; revoked keys stay listed as revoked.
    let sa = id
        .service_accounts()
        .create(&NewServiceAccount::new("ci"))
        .await
        .expect("service account");
    let key = id
        .service_accounts()
        .keys()
        .create(&sa.id, &NewApiKey::new("deploy", &["records:read"]).expires_in_days(30))
        .await
        .expect("key");
    assert!(!key.key.is_empty());
    let keys = id.service_accounts().keys().list(&sa.id).await.expect("keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].scopes, ["records:read"]);
    // The key works as a credential.
    let as_key = ForgeBase::builder(&s.base)
        .api_key(key.key.clone())
        .build()
        .expect("client");
    assert!(as_key.health().await);
    id.api_keys().revoke(&keys[0].id).await.expect("revoke");
    assert!(
        id.api_keys()
            .list()
            .await
            .expect("api keys")
            .iter()
            .any(|k| k.id == keys[0].id && k.revoked)
    );
    assert!(
        id.admins()
            .list()
            .await
            .expect("admins")
            .iter()
            .any(|a| a.email == common::ADMIN_EMAIL)
    );
    assert!(!id.audit(&Default::default()).await.expect("audit").is_empty());
    assert!(!id.rotate_signing_key().await.expect("rotate").is_empty());
    id.service_accounts().delete(&sa.id).await.expect("delete");
}

#[tokio::test(flavor = "multi_thread")]
async fn customize_branding_domains_templates_agreements_and_mail_provider() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;
    let c = fb.customize();

    let b = c.branding().get(None).await.expect("branding");
    let _ = b;
    c.branding()
        .put(
            &BrandingInput {
                display_name: Some("Acme".into()),
                accent_color: Some("#112233".into()),
                ..BrandingInput::default()
            },
            None,
        )
        .await
        .expect("put branding");
    let png: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    c.branding()
        .upload_asset(BrandingAsset::Logo, png, "image/png", None)
        .await
        .expect("upload");
    let logo = c.hosted().asset(BrandingAsset::Logo, None).await.expect("download");
    assert_eq!(&logo[..], png);
    assert!(c.hosted().theme(None).await.expect("theme").contains("#112233"));
    assert_eq!(c.hosted().branding(None).await.expect("public").display_name, "Acme");
    c.branding()
        .delete_asset(BrandingAsset::Logo, None)
        .await
        .expect("delete asset");

    let d = c.domains().add("auth.acme.example.test", None).await.expect("domain");
    assert!(
        c.domains()
            .list(None)
            .await
            .expect("domains")
            .iter()
            .any(|x| x.id == d.id)
    );
    let v = c.domains().verify(&d.id).await.expect("verify");
    assert!(!v.verified, "no DNS record was published");
    c.domains().delete(&d.id).await.expect("delete domain");

    let templates = c.email().templates(None).await.expect("templates");
    assert!(templates.iter().any(|t| t.key == "magic_link"));

    let ag = c
        .agreements()
        .publish(
            &AgreementInput {
                kind: "terms".into(),
                title: "Terms".into(),
                body: "be nice".into(),
                url: None,
            },
            None,
        )
        .await
        .expect("agreement");
    assert_eq!(ag.version, 1);
    assert_eq!(c.hosted().agreements(None).await.expect("public agreements").len(), 1);

    // Mail provider: the dev provider needs no credentials; a bad setting is named by the server.
    let before = c.email().provider(None).await.expect("provider");
    assert!(!before.configured);
    let put = c
        .email()
        .put_provider(&MailProviderConfig::new("log").from_address("noreply@acme.test"), None)
        .await
        .expect("put provider");
    assert!(put.configured);
    let bad = c
        .email()
        .put_provider(&MailProviderConfig::new("resend"), None)
        .await
        .expect_err("resend needs a key");
    assert_eq!((bad.status(), bad.code()), (Some(400), "invalid_mail_config"));
    let sent = c.email().test_provider("me@acme.test", None).await;
    assert!(sent.is_ok(), "the log provider accepts a test send: {sent:?}");
    c.email().delete_provider(None).await.expect("delete provider");
    assert!(
        c.email()
            .delete_provider(None)
            .await
            .expect_err("none left")
            .is_not_found()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn billing_and_entitlements_admin_models_decode_what_the_server_sends() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;
    let b = fb.billing();

    let settings = b.settings().get().await.expect("settings");
    let back = b.settings().put(&settings).await.expect("put settings unchanged");
    assert_eq!(
        serde_json::to_value(&back).expect("json"),
        serde_json::to_value(&settings).expect("json")
    );
    assert!(b.catalog().products().list().await.is_ok());
    assert!(b.catalog().prices().list(None).await.is_ok());
    assert!(b.catalog().coupons().list().await.is_ok());
    assert!(b.catalog().tax_rates().list().await.is_ok());
    assert!(b.providers().list().await.is_ok());
    assert!(b.customers().list(None).await.is_ok());
    assert!(b.dunning(Some(10)).await.expect("dunning").is_empty());
    let stats = b.stats(&StatsOptions::default()).await.expect("stats");
    assert!(stats.from < stats.to);
    assert!(b.usage_meters(&UsageMetersOptions::default()).await.is_ok());
    assert!(b.invoices().list(&ListInvoices::default()).await.is_ok());
    assert!(b.credit_notes().list(&SubjectListOptions::default()).await.is_ok());
    assert!(b.orders().list_all(&Default::default()).await.is_ok());
    assert!(b.webhook_deliveries().list(&Default::default()).await.is_ok());
    assert!(b.maintenance().await.is_ok());

    let e = fb.entitlements();
    for plan in e.plans().list().await.expect("plans") {
        let again = e.plans().get(&plan.key).await.expect("plan");
        assert_eq!(again.key, plan.key);
    }
    assert!(e.features().list().await.is_ok());
    assert!(e.addons().list().await.is_ok());
    assert!(e.usage_packs().list().await.is_ok());
    assert!(e.quota_policies().list().await.is_ok());
    assert!(e.overrides().list(None).await.is_ok());
    assert!(e.limits().list(None).await.is_ok());
    assert!(e.usage_events(&Default::default()).await.is_ok());
    let alerts = e.usage_alerts(Some("org:acme")).await;
    assert!(alerts.is_ok(), "{alerts:?}");
    assert!(e.maintenance().await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn backups_indexes_documents_and_system_reads() {
    let Some(s) = Server::start() else { return };
    let fb = s.admin().await;

    let sys = fb.system();
    let features = sys.features().await.expect("features");
    assert!(!features.features.is_empty());
    let snap = sys
        .backups()
        .create(&CreateSnapshot {
            label: Some("sdk".into()),
            include_indexes: Some(false),
            retention: None,
        })
        .await
        .expect("snapshot");
    assert!(
        sys.backups()
            .list()
            .await
            .expect("list")
            .iter()
            .any(|b| b.id == snap.id)
    );
    assert!(sys.backups().verify(&snap.id).await.expect("verify").ok);
    assert_eq!(sys.backups().get("latest").await.expect("latest").id, snap.id);
    assert_eq!(
        sys.backups()
            .prune(&Retention {
                keep_last: Some(5),
                max_age_days: None
            })
            .await
            .expect("prune")
            .len(),
        0
    );
    sys.backups().delete(&snap.id).await.expect("delete");
    let logins = sys.db_logins().list().await;
    assert!(logins.is_err(), "database logins are a PostgreSQL feature: {logins:?}");

    fb.collections()
        .create(&json!({"name": "articles", "fields": [{"name": "title", "type": "text"}, {"name": "views", "type": "number"}]}))
        .await
        .expect("collection");
    let ix = fb.indexes();
    let made = ix
        .create(
            &NewIndex::new("articles", vec![IndexKey::asc("title"), IndexKey::desc("views")])
                .name("idx_articles_title_views"),
        )
        .await
        .expect("index");
    assert_eq!(made.name, "idx_articles_title_views");
    assert!(
        ix.list(Some("articles"))
            .await
            .expect("list")
            .iter()
            .any(|i| i.name == made.name)
    );
    assert_eq!(ix.get(&made.name).await.expect("get").columns.len(), 2);
    ix.stats(&made.name).await.expect("stats");
    ix.rebuild(&made.name).await.expect("rebuild");
    ix.analyze(Some("articles")).await.expect("analyze");
    assert!(ix.recommendations().await.is_ok());
    let dup = ix
        .create(&NewIndex::new("articles", vec![IndexKey::asc("title")]).name("idx_articles_title_views"))
        .await
        .expect_err("name taken");
    assert!(dup.is_conflict());
    let t = ix
        .create(
            &NewIndex::new("articles", vec![IndexKey::asc("title")])
                .name("idx_articles_trgm")
                .kind(IndexKind::Trigram),
        )
        .await;
    assert!(t.is_ok(), "trigram indexes run on every engine (ADR-0034): {t:?}");
    assert_eq!(ix.set_slow_query_threshold(0).await.expect("threshold"), 0);
    fb.collection("articles")
        .list(&Default::default())
        .await
        .expect("a query to log");
    assert!(ix.slow_queries().await.expect("slow").threshold_ms == 0);
    ix.clear_slow_queries().await.expect("clear");
    ix.drop_index("idx_articles_trgm").await.expect("drop");

    let docs = fb.documents().collection("articles");
    let d = docs
        .create(&json!({"title": "Hello", "views": 1}))
        .await
        .expect("create");
    let id = d["id"].as_str().expect("id").to_owned();
    let got = docs.get(&id, &GetDocumentOptions::default()).await.expect("get");
    assert_eq!(got["title"], "Hello");
    let patched = docs
        .patch(
            &id,
            &json!({"views": 2}),
            WriteOptions {
                version: got["_version"].as_u64(),
            },
        )
        .await
        .expect("patch");
    assert_eq!(patched["views"].as_f64(), Some(2.0));
    let stale = docs
        .patch(
            &id,
            &json!({"views": 3}),
            WriteOptions {
                version: got["_version"].as_u64(),
            },
        )
        .await
        .expect_err("stale version");
    assert!(stale.is_conflict(), "{stale}");
    let many = docs
        .create_many(&[json!({"title": "a"}), json!({"title": "b"})])
        .await
        .expect("many");
    assert_eq!(many.len(), 2);
    assert!(docs.versions(&id).await.is_ok());
    docs.delete(&id, WriteOptions::default()).await.expect("delete");
    assert!(
        docs.get(&id, &GetDocumentOptions::default())
            .await
            .expect_err("deleted")
            .is_not_found()
    );
}
