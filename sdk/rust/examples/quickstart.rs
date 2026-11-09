//! Sign in, create a collection, write and query records.
//!
//!     FORGEBASE_URL=http://localhost:8090 FORGEBASE_EMAIL=me@example.com FORGEBASE_PASSWORD=... \
//!         cargo run --example quickstart

use forgebase_sdk::{ForgeBase, ListOptions, SignInOptions, SignInResult, WriteOptions};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct Post {
    id: String,
    title: String,
    views: f64,
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

#[tokio::main]
async fn main() -> forgebase_sdk::Result<()> {
    let fb = ForgeBase::new(env("FORGEBASE_URL", "http://localhost:8090"))?;

    // Sign in (use `sign_in_admin` for an administrator). The session is kept and refreshed for you.
    match fb
        .auth()
        .sign_in(
            &env("FORGEBASE_EMAIL", "me@example.com"),
            &env("FORGEBASE_PASSWORD", "a long passphrase"),
            &SignInOptions::default(),
        )
        .await?
    {
        SignInResult::Session(s) => println!("signed in, session {}", s.session_id),
        SignInResult::MfaRequired(c) => {
            // Ask the user for a TOTP code, then:
            let session = fb.auth().verify_mfa(&c.mfa_token, "123456").await?;
            println!("signed in with MFA, session {}", session.session_id);
        }
        _ => unreachable!("sign-in results are a session or an MFA challenge"),
    }

    // Collections are defined with JSON.
    let _ = fb
        .collections()
        .create(&json!({"name": "posts", "type": "base", "fields": [
            {"name": "title", "type": "text", "required": true},
            {"name": "views", "type": "number"}
        ]}))
        .await;

    // Records: untyped (`serde_json::Value`) or your own struct via `.typed::<T>()`.
    let posts = fb.collection("posts").typed::<Post>();
    let created = fb
        .collection("posts")
        .create(&json!({"title": "hello", "views": 1}))
        .await?;
    let id = created["id"].as_str().unwrap_or_default().to_owned();

    // Optimistic concurrency: the write fails with a conflict error if someone else changed the record.
    let version = created["_version"].as_u64();
    fb.collection("posts")
        .update(&id, &json!({"views": 2}), WriteOptions { version })
        .await?;

    let page = posts
        .list(
            &ListOptions::new()
                .filter("views >= {:min}")
                .params(json!({"min": 1}))
                .sort("-views")
                .per_page(20),
        )
        .await?;
    for p in &page.items {
        println!("{} {} ({} views)", p.id, p.title, p.views);
    }

    // Every record, page after page, lazily.
    let mut it = posts.iter(ListOptions::new().sort("title").per_page(100));
    while let Some(p) = it.next().await? {
        println!("- {}", p.title);
    }

    // Errors carry the server's code, status and request id.
    if let Err(e) = fb.collection("posts").create(&json!({"views": 1})).await {
        println!(
            "{e}  (validation: {}, fields: {:?})",
            e.is_validation(),
            e.api().map(|a| a.field_errors())
        );
    }
    fb.auth().sign_out().await
}
