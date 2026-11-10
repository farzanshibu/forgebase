import { describe, expect, it } from "vitest";
import { API_KEY_PLACEHOLDER, collection, object, snippets, vectorCollection, type SnippetKind } from "./snippets";

const URL = "https://fb.example.com";
const KINDS = Object.keys(snippets) as SnippetKind[];
const NAME: Record<SnippetKind, string> = {
  collection: "posts", record: "posts", bucket: "avatars", object: "avatars/me/pic.png", function: "hello-fn", topic: "orders",
  subscription: "billing", jobKind: "email.send", kvNamespace: "cache", vectorCollection: "docs", graph: "Person",
};

describe("snippets", () => {
  it.each(KINDS)("%s: all three languages carry the origin, the name and the key placeholder", (kind) => {
    const s = snippets[kind](URL, NAME[kind]);
    for (const lang of ["js", "rust", "curl"] as const) {
      const text = s[lang];
      expect(text, `${kind} ${lang}`).toContain(URL);
      expect(text, `${kind} ${lang}`).toContain(API_KEY_PLACEHOLDER);
      const shown = kind === "object" ? "pic.png" : NAME[kind];
      expect(text, `${kind} ${lang}`).toContain(shown);
    }
  });

  it("never contains anything that looks like a real key", () => {
    for (const kind of KINDS) {
      const s = snippets[kind](URL, NAME[kind]);
      for (const text of [s.js, s.rust, s.curl]) {
        expect(text).not.toMatch(/fb_[A-Za-z0-9]{8,}|sk_[A-Za-z0-9]{8,}|Bearer [A-Za-z0-9._-]{12,}/);
        // The only quoted key value is the placeholder.
        for (const m of text.matchAll(/(?:apiKey:|api_key\(|X-API-Key:)\s*"?([^")\s]+)/g)) expect(m[1]).toBe(API_KEY_PLACEHOLDER);
      }
    }
  });

  it("uses the JavaScript SDK client and the Rust crate", () => {
    const s = collection(URL, "posts");
    expect(s.js).toContain('import { ForgeBase } from "@forgebase/sdk"');
    expect(s.js).toContain(`new ForgeBase("${URL}"`);
    expect(s.js).toContain('fb.collection("posts").list(');
    expect(s.rust).toContain("forgebase_sdk");
    expect(s.curl).toContain("/api/v1/collections/posts/records");
  });

  it("encodes names for the path and keeps an object key's slashes", () => {
    expect(collection(URL, "a b").curl).toContain("/collections/a%20b/records");
    expect(object(URL, "media", "users/1/a b.png").curl).toContain("/objects/users/1/a%20b.png");
  });

  it("fits a vector example to the collection's field", () => {
    const s = vectorCollection(URL, "docs", "body", 2);
    expect(s.js).toContain('"field": "body"');
    expect(s.curl).toContain('"vector": [0.1, 0.2]');
  });
});
