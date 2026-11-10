//! Feature registry and runtime configuration (FB-91, ADR-0026), against a
//! real server: switched-off modules answer `404 feature_disabled` and start
//! nothing, `GET /api/system/features` lists every module with typed
//! settings and their sources, `PATCH /api/system/config` validates, refuses
//! overridden settings and writes the file atomically keeping its comments.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::path::Path;
use std::sync::Arc;

use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo};
use forgebase_core::config::{ConfigLoader, ConfigOverrides};
use forgebase_core::{Config, LoadedConfig};
use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_server::RunningServer;
use security_common::{ADMIN_PASSWORD, Resp, USER_PASSWORD, bearer, send};
use serde_json::{Value as Json, json};

/// A loaded configuration for a project in `dir`: the file there (if any),
/// `env` as the environment layer, port 0 from a flag.
fn load(dir: &Path, env: &[(&str, &str)]) -> LoadedConfig {
    let env = ConfigOverrides::from_env(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))).unwrap();
    let cli = ConfigOverrides {
        host: Some("127.0.0.1".into()),
        port: Some(0),
        ..ConfigOverrides::default()
    };
    ConfigLoader::new(dir).env(env).cli(cli).load().unwrap()
}

/// A fast-hashing auth service on the server's database.
async fn side_auth(server: &RunningServer, data_dir: &Path) -> AuthService {
    let mut acfg = AuthConfig::for_data_dir(data_dir);
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    AuthService::open_with_data(
        Arc::new(server.database().clone()),
        Arc::new(DefaultAuthorizer),
        data_dir,
        acfg,
        None,
    )
    .await
    .unwrap()
}

async fn admin(server: &RunningServer, data_dir: &Path, email: &str) -> String {
    side_auth(server, data_dir)
        .await
        .create_admin(email, ADMIN_PASSWORD, "test", &ClientInfo::default())
        .await
        .unwrap();
    let body = serde_json::to_vec(&json!({"email": email, "password": ADMIN_PASSWORD})).unwrap();
    let r = send(server.local_addr(), "POST", "/api/auth/admins/login", &[], &body).await;
    assert_eq!(r.status, 200, "admin login: {}", r.text());
    r.json()["access_token"].as_str().unwrap().to_owned()
}

async fn user(server: &RunningServer, data_dir: &Path, email: &str) -> String {
    side_auth(server, data_dir)
        .await
        .signup(&ClientInfo::default(), None, email, USER_PASSWORD, None)
        .await
        .unwrap();
    let body = serde_json::to_vec(&json!({"email": email, "password": USER_PASSWORD})).unwrap();
    let r = send(server.local_addr(), "POST", "/api/auth/login", &[], &body).await;
    assert_eq!(r.status, 200, "login: {}", r.text());
    r.json()["access_token"].as_str().unwrap().to_owned()
}

async fn call(server: &RunningServer, token: Option<&str>, method: &str, path: &str, body: Option<Json>) -> Resp {
    let auth = token.map(bearer);
    let mut headers = Vec::new();
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let bytes = body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default();
    send(server.local_addr(), method, path, &headers, &bytes).await
}

fn config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg
}

fn feature<'a>(view: &'a Json, id: &str) -> &'a Json {
    view["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == id)
        .unwrap_or_else(|| panic!("feature {id} listed"))
}

fn setting<'a>(view: &'a Json, key: &str) -> &'a Json {
    view["features"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["settings"].as_array().unwrap())
        .find(|s| s["key"] == key)
        .unwrap_or_else(|| panic!("setting {key} listed"))
}

/// One route of every switchable module that has routes.
const PROBES: &[(&str, &str, &str, &str)] = &[
    ("storage", "GET", "/api/v1/storage/buckets", "Object storage"),
    ("functions", "GET", "/api/v1/functions", "Server functions"),
    ("queues", "GET", "/api/v1/queues/topics", "Queues and topics"),
    ("realtime", "GET", "/api/v1/realtime/stats", "Realtime"),
    ("vector", "GET", "/api/v1/vector/collections", "Vector search"),
    ("graph", "GET", "/api/v1/graph/stats", "Graph"),
    (
        "identity",
        "GET",
        "/api/auth/oidc/default/.well-known/openid-configuration",
        "Identity provider",
    ),
    ("identity", "GET", "/api/auth/clients", "Identity provider"),
    ("identity", "POST", "/api/v1/realms", "Identity provider"),
    ("access", "GET", "/api/v1/orgs", "Access control"),
    ("access", "GET", "/api/v1/authz/me", "Access control"),
    ("billing", "GET", "/api/v1/billing/settings", "Billing"),
    ("billing", "POST", "/api/v1/billing/webhooks/stripe", "Billing"),
];

#[tokio::test]
async fn every_switchable_module_can_be_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.storage.enabled = false;
    cfg.functions.enabled = false;
    cfg.queue.enabled = false;
    cfg.realtime.enabled = false;
    cfg.vector.enabled = false;
    cfg.graph.enabled = false;
    cfg.identity.enabled = false;
    cfg.access.enabled = false;
    cfg.billing.enabled = false;
    assert!(!cfg.resp.enabled, "the Redis listener is off by default");
    let server = forgebase_server::start(&cfg).await.unwrap();
    let token = admin(&server, &cfg.data_dir, "off@example.com").await;

    for (id, method, path, name) in PROBES {
        for t in [None, Some(token.as_str())] {
            let r = call(&server, t, method, path, None).await;
            assert_eq!(r.status, 404, "{id}: {method} {path}: {}", r.text());
            let j = r.json();
            assert_eq!(j["error"]["code"], "feature_disabled", "{path}");
            assert_eq!(j["error"]["message"], format!("{name} is disabled on this server"));
            assert!(j["error"]["request_id"].as_str().is_some_and(|s| !s.is_empty()));
        }
    }
    // No services, workers or listeners.
    assert!(
        server.functions().is_none(),
        "no functions engine, no after-hook dispatcher"
    );
    assert!(server.vector().is_none(), "no vector engine, no warm-up or saver");
    assert!(server.queue().is_none(), "no queue maintenance or push task");
    assert!(server.billing().is_none(), "no billing maintenance");
    assert!(server.resp_addr().is_none(), "RESP listener not bound");
    assert!(!server.realtime().config().dispatch, "no realtime dispatcher");
    let schedules = server.jobs().schedules(&AccessContext::system()).await.unwrap();
    assert!(
        !schedules
            .iter()
            .any(|s| s.enabled && (s.name == "storage-reconcile" || s.name == "billing-maintenance")),
        "{schedules:?}"
    );

    // Always-on modules and the parts of auth that stay: not gated.
    for (method, path) in [
        ("GET", "/api/v1/collections"),
        ("GET", "/api/v1/jobs"),
        ("GET", "/api/v1/kv/keys"),
        ("GET", "/api/auth/realms/default"),
        ("GET", "/api/auth/realms/default/oauth-providers"),
        ("GET", "/api/auth/me"),
    ] {
        let r = call(&server, Some(&token), method, path, None).await;
        assert_ne!(r.code(), "feature_disabled", "{method} {path}: {}", r.text());
    }
    // Disabled modules are not documented.
    let doc = call(&server, None, "GET", "/api/openapi.json", None).await.text();
    for prefix in [
        "/api/v1/storage/",
        "/api/v1/functions",
        "/api/v1/queues/",
        "/api/v1/realtime/",
        "/api/v1/vector/",
        "/api/v1/graph/",
        "/api/auth/oidc/",
        "/api/auth/clients",
        "/api/v1/realms",
        "/api/v1/orgs",
        "/api/v1/billing/",
    ] {
        assert!(!doc.contains(&format!("\"{prefix}")), "{prefix} not documented");
    }
    assert!(doc.contains("\"/api/v1/collections\""));
    assert!(doc.contains("\"/api/auth/realms/{realm}\""));
    assert!(doc.contains("\"/api/system/features\""));

    // The registry reports them off.
    let view = call(&server, Some(&token), "GET", "/api/system/features", None).await;
    assert_eq!(view.status, 200, "{}", view.text());
    let view = view.json();
    for id in [
        "storage",
        "functions",
        "queues",
        "realtime",
        "vector",
        "graph",
        "identity",
        "access",
        "billing",
        "resp",
    ] {
        assert_eq!(feature(&view, id)["enabled"], false, "{id}");
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_module_turned_off_then_on_pauses_and_resumes_its_schedule() {
    let dir = tempfile::tempdir().unwrap();
    let reconcile = |server: &RunningServer| {
        let jobs = server.jobs().clone();
        async move {
            jobs.schedules(&AccessContext::system())
                .await
                .unwrap()
                .into_iter()
                .find(|s| s.name == forgebase_storage::RECONCILE_SCHEDULE)
                .map(|s| s.enabled)
        }
    };
    let server = forgebase_server::start(&config(dir.path())).await.unwrap();
    assert_eq!(reconcile(&server).await, Some(true));
    server.shutdown().await.unwrap();

    let mut off = config(dir.path());
    off.storage.enabled = false;
    let server = forgebase_server::start(&off).await.unwrap();
    assert_eq!(reconcile(&server).await, Some(false), "paused while storage is off");
    let r = call(&server, None, "GET", "/api/v1/storage/buckets", None).await;
    assert_eq!(r.code(), "feature_disabled");
    server.shutdown().await.unwrap();

    let server = forgebase_server::start(&config(dir.path())).await.unwrap();
    assert_eq!(reconcile(&server).await, Some(true), "resumed");
    let r = call(&server, None, "GET", "/api/v1/storage/buckets", None).await;
    assert_eq!(r.status, 200, "mounted again (anonymous sees no buckets): {}", r.text());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn registry_lists_every_module_with_typed_settings_and_sources() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("forgebase.toml");
    std::fs::write(
        &file,
        "# my settings\n[server]\nrequest_timeout_secs = 45 # longer\n\n[storage]\nmax_object_bytes = 1048576\n",
    )
    .unwrap();
    let secret = "not-a-real-secret-value-9f1c";
    let loaded = load(
        dir.path(),
        &[
            ("FORGEBASE_LOG_FORMAT", "json"),
            ("FORGEBASE_STORAGE_S3_SECRET_KEY", secret),
        ],
    );
    let server = forgebase_server::start_loaded(&loaded).await.unwrap();
    let token = admin(&server, &loaded.config.data_dir, "reg@example.com").await;
    let r = call(&server, Some(&token), "GET", "/api/system/features", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert!(!r.text().contains(secret), "secret values are never listed");
    let view = r.json();
    assert_eq!(view["config_file"], file.display().to_string());
    assert_eq!(view["restart_pending"], false);

    let ids: Vec<&str> = view["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    for id in [
        "server",
        "database",
        "auth",
        "storage",
        "functions",
        "jobs",
        "queues",
        "realtime",
        "vector",
        "graph",
        "identity",
        "access",
        "resp",
        "billing",
    ] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    for f in view["features"].as_array().unwrap() {
        assert_eq!(f["always_on"], !f["can_disable"].as_bool().unwrap(), "{}", f["id"]);
        if f["can_disable"] == true {
            let first = &f["settings"][0];
            assert!(first["key"].as_str().unwrap().ends_with(".enabled"), "{}", f["id"]);
            assert_eq!(first["type"], "bool");
        }
        for s in f["settings"].as_array().unwrap() {
            for field in [
                "key",
                "label",
                "help",
                "env",
                "flag",
                "type",
                "unit",
                "options",
                "value",
                "default",
                "source",
                "editable",
                "restart_required",
                "secret",
            ] {
                assert!(s.get(field).is_some(), "{} has {field}", s["key"]);
            }
        }
    }
    let jobs = feature(&view, "jobs");
    assert_eq!(jobs["can_disable"], false, "every module's maintenance runs on jobs");
    assert_eq!(feature(&view, "queues")["settings"][0]["key"], "queue.enabled");
    let storage = feature(&view, "storage");
    assert_eq!(storage["name"], "Object storage");
    assert_eq!(storage["studio_path"], "/storage");
    assert_eq!(storage["enabled"], true);

    // Sources: default, file, env, flag.
    let s = setting(&view, "storage.max_object_bytes");
    assert_eq!(
        (s["value"].clone(), s["source"].clone()),
        (json!(1_048_576), json!("file"))
    );
    assert_eq!(s["default"], json!(5_368_709_120_u64));
    assert_eq!((s["type"].clone(), s["unit"].clone()), (json!("int"), json!("bytes")));
    assert_eq!(s["editable"], true);
    let s = setting(&view, "server.request_timeout_secs");
    assert_eq!((s["value"].clone(), s["source"].clone()), (json!(45), json!("file")));
    let s = setting(&view, "log.format");
    assert_eq!((s["value"].clone(), s["source"].clone()), (json!("json"), json!("env")));
    assert_eq!(s["editable"], false, "the file cannot override the environment");
    assert_eq!(s["options"], json!(["pretty", "json"]));
    let s = setting(&view, "server.port");
    assert_eq!((s["value"].clone(), s["source"].clone()), (json!(0), json!("flag")));
    assert_eq!(s["editable"], false);
    let s = setting(&view, "database.read_pool_size");
    assert_eq!((s["value"].clone(), s["source"].clone()), (json!(8), json!("default")));
    assert_eq!(s["options"], Json::Null);
    let s = setting(&view, "resp.enabled");
    assert_eq!((s["value"].clone(), s["default"].clone()), (json!(false), json!(false)));
    // Secrets: no value, not editable, the variable named in help.
    let s = setting(&view, "storage.s3_secret_key");
    assert_eq!((s["value"].clone(), s["secret"].clone()), (Json::Null, json!(true)));
    assert_eq!(
        (s["editable"].clone(), s["source"].clone()),
        (json!(false), json!("env"))
    );
    assert!(s["help"].as_str().unwrap().contains("FORGEBASE_STORAGE_S3_SECRET_KEY"));
    let s = setting(&view, "auth.master_key");
    assert_eq!((s["value"].clone(), s["editable"].clone()), (Json::Null, json!(false)));
    // Server-wide settings sit under `server`.
    let server_keys: Vec<&str> = feature(&view, "server")["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    for k in ["data_dir", "server.host", "log.level", "database.synchronous"] {
        assert!(server_keys.contains(&k), "{k}");
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn feature_and_config_api_is_for_administrators_only() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(dir.path(), &[]);
    let server = forgebase_server::start_loaded(&loaded).await.unwrap();
    let data_dir = &loaded.config.data_dir;
    let token = admin(&server, data_dir, "boss@example.com").await;
    let member = user(&server, data_dir, "member@example.com").await;
    let patch = json!({"set": {"graph.enabled": false}});
    // Anonymous, even on loopback where ops_access lets /api/system/info through.
    let info = call(&server, None, "GET", "/api/system/info", None).await;
    assert_eq!(info.status, 200);
    let r = call(&server, None, "GET", "/api/system/features", None).await;
    assert_eq!((r.status, r.code()), (401, "unauthenticated".to_owned()));
    let r = call(&server, None, "PATCH", "/api/system/config", Some(patch.clone())).await;
    assert_eq!(r.status, 401);
    let r = call(&server, Some(&member), "GET", "/api/system/features", None).await;
    assert_eq!((r.status, r.code()), (403, "forbidden".to_owned()), "{}", r.text());
    let r = call(
        &server,
        Some(&member),
        "PATCH",
        "/api/system/config",
        Some(patch.clone()),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert!(!dir.path().join("forgebase.toml").exists(), "nothing written");
    let r = call(&server, Some(&token), "PATCH", "/api/system/config", Some(patch)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn patch_writes_the_file_keeps_comments_and_applies_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("forgebase.toml");
    let original = forgebase_core::config::CONFIG_TEMPLATE;
    std::fs::write(&file, original).unwrap();
    let loaded = load(dir.path(), &[]);
    let server = forgebase_server::start_loaded(&loaded).await.unwrap();
    let data_dir = loaded.config.data_dir.clone();
    let token = admin(&server, &data_dir, "cfg@example.com").await;

    let r = call(
        &server,
        Some(&token),
        "PATCH",
        "/api/system/config",
        Some(json!({"set": {
            "storage.enabled": false,
            "database.read_pool_size": 4,
            "realtime.max_connections_per_principal": 8,
        }})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!(j["written"], file.display().to_string());
    assert_eq!(j["restart_required"], true);
    assert_eq!(j["applied"], json!([]), "nothing applies without a restart");
    assert_eq!(setting(&j, "storage.enabled")["value"], false);
    assert_eq!(setting(&j, "storage.enabled")["source"], "file");
    assert_eq!(setting(&j, "database.read_pool_size")["value"], 4);

    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# ForgeBase configuration.\n"), "{text}");
    assert!(
        text.contains("read_pool_size = 4          # FORGEBASE_DB_READ_POOL_SIZE"),
        "{text}"
    );
    assert!(text.contains("max_connections_per_principal = 8  # FORGEBASE_REALTIME_MAX_CONNECTIONS_PER_PRINCIPAL"));
    assert!(text.contains("# \"full\" fsyncs the WAL on every commit (durable across power loss)."));
    assert!(text.contains("[storage]\nenabled = false"), "{text}");

    // Still running the old configuration until a restart.
    let r = call(&server, Some(&token), "GET", "/api/v1/storage/buckets", None).await;
    assert_eq!(r.status, 200, "storage still mounted: {}", r.text());
    let view = call(&server, Some(&token), "GET", "/api/system/features", None)
        .await
        .json();
    assert_eq!(view["restart_pending"], true);
    assert_eq!(feature(&view, "storage")["enabled"], false, "the configured value");
    server.shutdown().await.unwrap();

    let loaded = load(dir.path(), &[]);
    assert!(!loaded.config.storage.enabled);
    assert_eq!(loaded.config.database.read_pool_size, 4);
    let server = forgebase_server::start_loaded(&loaded).await.unwrap();
    let r = call(&server, Some(&token), "GET", "/api/v1/storage/buckets", None).await;
    assert_eq!(r.code(), "feature_disabled", "applied after the restart");
    let view = call(&server, Some(&token), "GET", "/api/system/features", None)
        .await
        .json();
    assert_eq!(view["restart_pending"], false);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn patch_validates_everything_and_refuses_overridden_settings() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("forgebase.toml");
    std::fs::write(&file, "# keep me\n[log]\nlevel = \"info\"\n").unwrap();
    let loaded = load(dir.path(), &[("FORGEBASE_DB_READ_POOL_SIZE", "4")]);
    let server = forgebase_server::start_loaded(&loaded).await.unwrap();
    let token = admin(&server, &loaded.config.data_dir, "val@example.com").await;
    let before = std::fs::read_to_string(&file).unwrap();

    let r = call(
        &server,
        Some(&token),
        "PATCH",
        "/api/system/config",
        Some(json!({"set": {
            "storage.enabled": "no",
            "log.format": "xml",
            "server.request_timeout_secs": 0,
            "storage.s3_secret_key": "x",
            "no.such": 1,
        }})),
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (400, "validation_failed".to_owned()),
        "{}",
        r.text()
    );
    let d = &r.json()["error"]["details"];
    assert_eq!(d["storage.enabled"], "must be true or false");
    assert_eq!(d["log.format"], "must be one of pretty or json");
    assert!(d["no.such"].as_str().unwrap().starts_with("is not a setting"));
    assert!(
        d["storage.s3_secret_key"]
            .as_str()
            .unwrap()
            .contains("environment variable")
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");

    let r = call(
        &server,
        Some(&token),
        "PATCH",
        "/api/system/config",
        Some(json!({"set": {"server.request_timeout_secs": 0}})),
    )
    .await;
    assert_eq!(r.status, 400);
    assert_eq!(
        r.json()["error"]["details"]["server.request_timeout_secs"],
        "must be a whole number from 1 to 3600 seconds, such as 30"
    );

    for key in ["database.read_pool_size", "server.port"] {
        let r = call(
            &server,
            Some(&token),
            "PATCH",
            "/api/system/config",
            Some(json!({"set": {key: 2, "graph.enabled": false}})),
        )
        .await;
        assert_eq!(
            (r.status, r.code()),
            (409, "setting_overridden".to_owned()),
            "{key}: {}",
            r.text()
        );
        let reason = r.json()["error"]["details"][key].as_str().unwrap().to_owned();
        assert!(reason.contains("FORGEBASE_") || reason.contains("--port"), "{reason}");
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");
    let r = call(
        &server,
        Some(&token),
        "PATCH",
        "/api/system/config",
        Some(json!({"bogus": 1})),
    )
    .await;
    assert_eq!(r.status, 400, "{}", r.text());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn patch_creates_a_config_file_next_to_the_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    // Built in code, no file: values are the running ones, sources default.
    let cfg = config(dir.path());
    let server = forgebase_server::start(&cfg).await.unwrap();
    let token = admin(&server, &cfg.data_dir, "new@example.com").await;
    let view = call(&server, Some(&token), "GET", "/api/system/features", None)
        .await
        .json();
    let target = dir.path().join("forgebase.toml");
    assert_eq!(view["config_file"], target.display().to_string());
    assert_eq!(setting(&view, "server.port")["value"], 0);
    assert_eq!(setting(&view, "server.port")["source"], "default");
    let r = call(
        &server,
        Some(&token),
        "PATCH",
        "/api/system/config",
        Some(json!({"set": {"vector.enabled": false}})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "[vector]\nenabled = false\n");
    server.shutdown().await.unwrap();
}
