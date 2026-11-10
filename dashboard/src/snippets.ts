/**
 * "Use from code" snippets (dashboard-ux.md rule 3): one generator per resource, each returning JavaScript SDK,
 * Rust SDK and curl text, filled in with the server URL and the resource name. Keys are always the placeholder
 * `<YOUR_API_KEY>`; a generator never takes a real key.
 *
 * Usage:
 *   const code = snippets.collection(serverUrl(), "posts");   // { js, rust, curl }
 *   <UseFromCode snippet={code} />                            // or <UseFromCode kind="collection" name="posts" />
 *
 * JS uses `@forgebase/sdk` (`new ForgeBase(url)`); where it has no wrapper (storage, functions, key-value,
 * vector, graph) it uses `fb.request(path)`. Rust uses the `forgebase-sdk` crate. Shapes follow docs/sdk.md,
 * sdk/js/README.md and sdk/rust/README.md.
 */

export const API_KEY_PLACEHOLDER = "<YOUR_API_KEY>";

export interface Snippet {
  js: string;
  rust: string;
  curl: string;
}

export type SnippetKind = "collection" | "record" | "bucket" | "object" | "function" | "topic" | "subscription" | "jobKind" | "kvNamespace" | "vectorCollection" | "graph";

/** The server's base URL as the browser sees it (the dashboard is served by the server, under /admin/). */
export function serverUrl(): string {
  return typeof location !== "undefined" ? location.origin : "http://localhost:8090";
}

const q = (s: string) => JSON.stringify(s);
/** Path segment, encoded the way the server expects. */
const seg = (s: string) => encodeURIComponent(s);
/** Object keys keep their slashes. */
const keyPath = (k: string) => k.split("/").map(encodeURIComponent).join("/");
/** Single-quote a shell argument. */
const sh = (s: string) => `'${s.replace(/'/g, `'\\''`)}'`;

const jsClient = (url: string) => `import { ForgeBase } from "@forgebase/sdk";

const fb = new ForgeBase(${q(url)}, { apiKey: "${API_KEY_PLACEHOLDER}" });`;

const rustClient = (url: string, uses: string[] = []) => `use forgebase_sdk::{${["ForgeBase", ...uses].join(", ")}};

let fb = ForgeBase::builder(${q(url)})
    .api_key("${API_KEY_PLACEHOLDER}")
    .build()?;`;

const curlKey = `-H "X-API-Key: ${API_KEY_PLACEHOLDER}"`;
const curlJson = `${curlKey} -H "Content-Type: application/json"`;

/** List records of a collection. */
export function collection(url: string, name: string): Snippet {
  return {
    js: `${jsClient(url)}

const page = await fb.collection(${q(name)}).list({ perPage: 20, sort: ["-created"] });
console.log(page.items);`,
    rust: `${rustClient(url, ["ListOptions"])}

let page = fb.collection(${q(name)})
    .list(&ListOptions::new().sort("-created").per_page(20))
    .await?;
println!("{} record(s)", page.items.len());`,
    curl: `curl ${curlKey} \\
  "${url}/api/v1/collections/${seg(name)}/records?per_page=20&sort=-created"`,
  };
}

/** Read one record (or create one when `id` is omitted). */
export function record(url: string, name: string, id?: string): Snippet {
  if (!id) {
    return {
      js: `${jsClient(url)}

const created = await fb.collection(${q(name)}).create({ /* your fields */ });`,
      rust: `${rustClient(url)}
use serde_json::json;

let created = fb.collection(${q(name)}).create(&json!({ /* your fields */ })).await?;`,
      curl: `curl -X POST ${curlJson} \\
  -d '{}' "${url}/api/v1/collections/${seg(name)}/records"`,
    };
  }
  return {
    js: `${jsClient(url)}

const rec = await fb.collection(${q(name)}).get(${q(id)});`,
    rust: `${rustClient(url)}

let rec = fb.collection(${q(name)}).get(${q(id)}).await?;`,
    curl: `curl ${curlKey} \\
  "${url}/api/v1/collections/${seg(name)}/records/${seg(id)}"`,
  };
}

/** Upload a file to a bucket and list what is in it. */
export function bucket(url: string, name: string): Snippet {
  return {
    js: `${jsClient(url)}

// Upload a file (a Blob, File or string body)
await fb.request(${q(`/api/v1/storage/buckets/${seg(name)}/objects/hello.txt`)}, {
  method: "PUT",
  rawBody: "hello",
  headers: { "Content-Type": "text/plain" },
});

// List the files in the bucket
const page = await fb.request(${q(`/api/v1/storage/buckets/${seg(name)}/objects`)});`,
    rust: `${rustClient(url, ["ListObjects", "PutOptions"])}

let st = fb.storage();
st.put_object(${q(name)}, "hello.txt", "hello", &PutOptions::default()).await?;
let page = st.list_objects(${q(name)}, &ListObjects::default()).await?;`,
    curl: `curl -X PUT ${curlKey} -H "Content-Type: text/plain" \\
  --data-binary @hello.txt "${url}/api/v1/storage/buckets/${seg(name)}/objects/hello.txt"

curl ${curlKey} "${url}/api/v1/storage/buckets/${seg(name)}/objects"`,
  };
}

/** Download one object. */
export function object(url: string, bucketName: string, key: string): Snippet {
  const path = `/api/v1/storage/buckets/${seg(bucketName)}/objects/${keyPath(key)}`;
  return {
    js: `${jsClient(url)}

const res = await fb.request(${q(path)}, { raw: true });
const blob = await res.blob();`,
    rust: `${rustClient(url)}

let bytes = fb.storage().get_object(${q(bucketName)}, ${q(key)}, None).await?;`,
    curl: `curl ${curlKey} -o ${sh(key.split("/").pop() || "download")} \\
  "${url}${path}"`,
  };
}

/** Invoke a deployed function. */
export function fn(url: string, name: string): Snippet {
  return {
    js: `${jsClient(url)}

const out = await fb.request(${q(`/api/v1/functions/${seg(name)}/invoke`)}, {
  method: "POST",
  body: { hello: "world" },
});`,
    rust: `${rustClient(url)}
use serde_json::json;

let out = fb.functions().invoke(${q(name)}, &json!({ "hello": "world" })).await?;`,
    curl: `curl -X POST ${curlJson} \\
  -d '{"hello":"world"}' "${url}/api/v1/functions/${seg(name)}/invoke"`,
  };
}

/** Publish to a queue topic. */
export function topic(url: string, name: string): Snippet {
  return {
    js: `${jsClient(url)}

await fb.queues.publish(${q(name)}, { hello: "world" });`,
    rust: `${rustClient(url, ["NewMessage"])}
use serde_json::json;

fb.queues().publish(${q(name)}, &NewMessage::new(json!({ "hello": "world" }))).await?;`,
    curl: `curl -X POST ${curlJson} \\
  -d '{"messages":[{"data":{"hello":"world"}}]}' \\
  "${url}/api/v1/queues/topics/${seg(name)}/publish"`,
  };
}

/** Pull and acknowledge messages from a subscription. */
export function subscription(url: string, name: string): Snippet {
  return {
    js: `${jsClient(url)}

const messages = await fb.queues.pull(${q(name)}, { maxMessages: 10, waitMs: 2000 });
await fb.queues.ack(${q(name)}, messages.map((m) => m.ack_id));`,
    rust: `${rustClient(url, ["PullOptions"])}

let q = fb.queues();
for m in q.pull(${q(name)}, &PullOptions { max_messages: Some(10), ..Default::default() }).await? {
    q.ack(${q(name)}, &[m.ack_id]).await?;
}`,
    curl: `curl -X POST ${curlJson} \\
  -d '{"max_messages":10}' "${url}/api/v1/queues/subscriptions/${seg(name)}/pull"`,
  };
}

/** Enqueue a job of a kind. */
export function jobKind(url: string, kind: string): Snippet {
  return {
    js: `${jsClient(url)}

const job = await fb.jobs.enqueue(${q(kind)}, { id: 1 });
const done = await fb.jobs.wait(job.id);`,
    rust: `${rustClient(url, ["generated::NewJob"])}
use serde_json::json;

let job = fb.jobs().enqueue(&NewJob {
    kind: ${q(kind)}.into(),
    payload: Some(json!({ "id": 1 })),
    ..Default::default()
}).await?;`,
    curl: `curl -X POST ${curlJson} \\
  -d '{"kind":${q(kind)},"payload":{"id":1}}' "${url}/api/v1/jobs"`,
  };
}

/** Set and read a key in a key-value namespace. */
export function kvNamespace(url: string, ns: string): Snippet {
  const base = `/api/v1/kv/${seg(ns)}`;
  return {
    js: `${jsClient(url)}

await fb.request(${q(`${base}/strings/greeting`)}, { method: "PUT", body: { value: "hello", ttl_ms: 60000 } });
const key = await fb.request(${q(`${base}/keys/greeting`)});`,
    rust: `${rustClient(url, ["SetOptions"])}

let kv = fb.kv(${q(ns)});
kv.set("greeting", "hello", &SetOptions { ttl_ms: Some(60_000), ..Default::default() }).await?;
let value = kv.get_string("greeting").await?;`,
    curl: `curl -X PUT ${curlJson} \\
  -d '{"value":"hello","ttl_ms":60000}' "${url}${base}/strings/greeting"

curl ${curlKey} "${url}${base}/keys/greeting"`,
  };
}

/** Similarity search in a vector collection. */
export function vectorCollection(url: string, name: string, field = "embedding", dimension = 3): Snippet {
  const vec = `[${Array.from({ length: Math.min(Math.max(dimension, 1), 4) }, (_, i) => (0.1 * (i + 1)).toFixed(1)).join(", ")}${dimension > 4 ? ", …" : ""}]`;
  const body = `{ "field": ${q(field)}, "vector": ${vec}, "k": 5 }`;
  return {
    js: `${jsClient(url)}

const { hits } = await fb.request(${q(`/api/v1/vector/collections/${seg(name)}/search`)}, {
  method: "POST",
  body: ${body},
});`,
    rust: `${rustClient(url)}
use serde_json::json;

let hits = fb.request("POST", ${q(`/api/v1/vector/collections/${seg(name)}/search`)},
    Some(json!(${body}))).await?;`,
    curl: `curl -X POST ${curlJson} \\
  -d '${body.replace(/ …/, "")}' "${url}/api/v1/vector/collections/${seg(name)}/search"`,
  };
}

/** List graph nodes with a label (or all nodes) and walk from one. */
export function graph(url: string, label?: string): Snippet {
  const list = `/api/v1/graph/nodes${label ? `?label=${seg(label)}` : ""}`;
  return {
    js: `${jsClient(url)}

const nodes = await fb.request(${q(list)});
const walk = await fb.request("/api/v1/graph/traverse", {
  method: "POST",
  body: { start: nodes.items[0].id, max_depth: 2 },
});`,
    rust: `${rustClient(url)}

let nodes = fb.request("GET", ${q(list)}, None).await?;`,
    curl: `curl ${curlKey} "${url}${list}"`,
  };
}

/** Every generator, by resource kind, for `<UseFromCode kind=… name=… />`. */
export const snippets = {
  collection,
  record: (url: string, name: string) => record(url, name),
  bucket,
  object: (url: string, name: string) => {
    const [b, ...rest] = name.split("/");
    return object(url, b ?? name, rest.join("/") || "file");
  },
  function: fn,
  topic,
  subscription,
  jobKind,
  kvNamespace,
  vectorCollection: (url: string, name: string) => vectorCollection(url, name),
  graph: (url: string, name: string) => graph(url, name || undefined),
} satisfies Record<SnippetKind, (url: string, name: string) => Snippet>;

export default snippets;
