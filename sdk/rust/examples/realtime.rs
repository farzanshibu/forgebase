//! Watch a collection and a channel; resume from a stored cursor.
//!
//!     FORGEBASE_URL=http://localhost:8090 FORGEBASE_API_KEY=fb_... cargo run --example realtime

use std::time::Duration;

use forgebase_sdk::{ForgeBase, RealtimeEvent, SubscribeOptions};
use serde_json::json;

#[tokio::main]
async fn main() -> forgebase_sdk::Result<()> {
    let url = std::env::var("FORGEBASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let mut builder = ForgeBase::builder(url);
    if let Ok(key) = std::env::var("FORGEBASE_API_KEY") {
        builder = builder.api_key(key);
    }
    let fb = builder.build()?;
    let rt = fb.realtime();

    // `subscribe` resolves once the server accepted the subscription (WebSocket, falling back to SSE).
    let mut posts = rt
        .subscribe(SubscribeOptions::collection("posts").filter("views > 0"))
        .await?;
    let mut room = rt.subscribe(SubscribeOptions::channel("room")).await?;
    rt.publish("room", json!({"hello": "from rust"})).await?;

    while let Some(event) = posts.next_timeout(Duration::from_secs(5)).await {
        match event {
            RealtimeEvent::Record(r) => println!("{} {} {} cursor={}", r.kind, r.collection, r.record_id, r.cursor),
            RealtimeEvent::Reset(r) => println!(
                "replay impossible ({}): reload your state, continue from {}",
                r.reason, r.cursor
            ),
            RealtimeEvent::Error { code, message } => {
                println!("subscription ended: {code}: {message}");
                break;
            }
            other => println!("{other:?}"),
        }
    }
    if let Some(RealtimeEvent::Message(m)) = room.next_timeout(Duration::from_secs(1)).await {
        println!("channel message: {}", m.data);
    }

    // Store the cursor (a file, a database) and resume after a restart without losing or repeating events:
    let cursor = posts.cursor();
    drop(posts);
    if let Some(c) = cursor {
        let _resumed = rt
            .subscribe(SubscribeOptions::collection("posts").from_cursor(c))
            .await?;
    }
    rt.close();
    Ok(())
}
