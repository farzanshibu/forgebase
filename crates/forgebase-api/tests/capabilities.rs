//! The capability matrix (ADR-0034): `GET /api/system/capabilities` and
//! `database.capabilities` of `/api/system/info` say, per feature, which
//! engine runs it and in which state, following the backend and the
//! `[database.postgres.extensions]` modes. Runs on the backend
//! `FORGEBASE_TEST_BACKEND` selects.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::Api;
use forgebase_core::config::{ExtensionMode, PostgresExtensions};
use forgebase_db::{BackendKind, ExtensionSetup};
use serde_json::Value as Json;

fn feature<'a>(m: &'a Json, name: &str) -> &'a Json {
    m["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["feature"] == name)
        .unwrap_or_else(|| panic!("no {name} in {m}"))
}

fn modes(m: ExtensionMode) -> PostgresExtensions {
    PostgresExtensions {
        vector: m,
        graph: m,
        queue: m,
        cron: ExtensionMode::Off,
        trigram: m,
    }
}

#[tokio::test]
async fn capabilities_follow_the_backend_and_the_extension_modes() {
    let api = Api::admin().await;
    let pg = api.db.kind() == BackendKind::Postgres;
    let m = api.get("/api/system/capabilities").await.expect(StatusCode::OK).json();
    assert_eq!(m["backend"], if pg { "postgres" } else { "sqlite" });
    let names: Vec<&str> = m["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["feature"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "vector",
            "graph",
            "opencypher",
            "fulltext",
            "fuzzy",
            "queue",
            "cron",
            "kv",
            "realtime",
            "read_replicas",
            "sharding"
        ]
    );
    assert_eq!(feature(&m, "graph")["state"], "native");
    assert_eq!(feature(&m, "graph")["limits"]["max_depth"], 10);
    // The same matrix is part of the system info.
    let info = api.get("/api/system/info").await.expect(StatusCode::OK).json();
    assert_eq!(info["database"]["capabilities"], m["features"]);

    if !pg {
        for name in ["vector", "opencypher", "fuzzy", "queue", "cron"] {
            let f = feature(&m, name);
            assert_eq!(
                (&f["state"], &f["accelerated"]),
                (&Json::from("portable"), &Json::from(false)),
                "{f}"
            );
            assert!(f["engine"].as_str().unwrap().starts_with("portable"), "{f}");
        }
        assert_eq!(feature(&m, "fulltext")["engine"], "FTS5 (unicode61)");
        assert_eq!(feature(&m, "fuzzy")["limits"]["max_chars"], 4096);
        assert_eq!(
            feature(&m, "opencypher")["limits"]["max_paths_per_relationship"],
            100_000
        );
        assert_eq!(feature(&m, "read_replicas")["state"], "unavailable");
        return;
    }

    // PostgreSQL, extensions off: every accelerated feature is portable
    // because of its mode.
    api.db
        .setup_extensions(&ExtensionSetup {
            modes: modes(ExtensionMode::Off),
            ..ExtensionSetup::default()
        })
        .await
        .unwrap();
    let m = api.get("/api/system/capabilities").await.expect(StatusCode::OK).json();
    for name in ["vector", "opencypher", "fuzzy", "queue", "cron"] {
        let f = feature(&m, name);
        assert_eq!(
            (&f["state"], &f["mode"]),
            (&Json::from("off"), &Json::from("off")),
            "{f}"
        );
        assert!(f["engine"].as_str().unwrap().starts_with("portable"), "{f}");
    }
    assert_eq!(
        feature(&m, "fuzzy")["engine"],
        "portable (GIN over forgebase._forgebase_trgm)"
    );
    assert_eq!(feature(&m, "read_replicas")["state"], "native");

    // `auto`: pg_trgm (contrib, in every PostgreSQL) becomes active; AGE is
    // not loaded in the test sessions, so openCypher stays portable and says
    // why.
    api.db
        .setup_extensions(&ExtensionSetup {
            modes: PostgresExtensions {
                vector: ExtensionMode::Off,
                graph: ExtensionMode::Auto,
                queue: ExtensionMode::Off,
                cron: ExtensionMode::Off,
                trigram: ExtensionMode::Auto,
            },
            ..ExtensionSetup::default()
        })
        .await
        .unwrap();
    let m = api.get("/api/system/capabilities").await.expect(StatusCode::OK).json();
    let f = feature(&m, "fuzzy");
    assert_eq!(
        (&f["state"], &f["accelerated"], &f["mode"]),
        (&Json::from("active"), &Json::from(true), &Json::from("auto")),
        "{f}"
    );
    assert!(f["engine"].as_str().unwrap().starts_with("pg_trgm "), "{f}");
    let c = feature(&m, "opencypher");
    assert_eq!(
        (&c["state"], &c["mode"]),
        (&Json::from("portable"), &Json::from("auto")),
        "{c}"
    );
    assert!(c["reason"].as_str().is_some(), "{c}");
}
