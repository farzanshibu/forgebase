import { describe, expect, it } from "vitest";
import { MCP_SCOPE, buildScopes, describeScope, emptyChoice, parseScopes, readExample, summarizeScopes } from "./scopes";

const choice = (area: string, o: Partial<ReturnType<typeof emptyChoice>>) => ({ ...emptyChoice(area), ...o });

describe("buildScopes", () => {
  it("grants every resource of an area with <area>:<access>", () => {
    expect(buildScopes([choice("collections", { read: true })])).toEqual(["collections:read"]);
    expect(buildScopes([choice("storage", { read: true, write: true })])).toEqual(["storage:read", "storage:write"]);
  });
  it("names each picked resource: collections:read:<name>", () => {
    expect(buildScopes([choice("collections", { read: true, narrow: true, only: ["notes", "posts"] })])).toEqual(["collections:read:notes", "collections:read:posts"]);
  });
  it("grants nothing, never everything, while the person is still picking resources", () => {
    expect(buildScopes([choice("collections", { read: true, narrow: true })])).toEqual([]);
  });
  it("write does not imply read", () => {
    expect(buildScopes([choice("kv", { write: true })])).toEqual(["kv:write"]);
  });
  it("skips areas with nothing ticked and keeps the order of the areas", () => {
    expect(buildScopes([choice("collections", {}), choice("jobs", { read: true }), choice("kv", { read: true })])).toEqual(["jobs:read", "kv:read"]);
  });
});

describe("parseScopes", () => {
  it("is the inverse of buildScopes for what the dialog can show, and ignores the rest", () => {
    const scopes = ["collections:read:notes", "storage:write", "*", "auth:*"];
    expect(buildScopes(parseScopes(scopes))).toEqual(["collections:read:notes", "storage:write"]);
  });
});

describe("describeScope", () => {
  it("says what a scope lets a key do", () => {
    expect(describeScope("collections:read:notes")).toBe("Read records in notes");
    expect(describeScope("storage:write")).toBe("Upload and delete files, all buckets");
    expect(describeScope("*")).toBe("Full access to everything");
    expect(describeScope("weird:thing")).toBe("weird:thing");
  });
});

describe("readExample", () => {
  it("shows a curl that reads the picked collection with the placeholder key", () => {
    const e = readExample(["collections:read:notes"], "http://x");
    expect(e?.curl).toContain("http://x/api/v1/collections/notes/records");
    expect(e?.curl).toContain("<YOUR_API_KEY>");
  });
  it("has nothing to read for a write-only key", () => {
    expect(readExample(["storage:write"], "http://x")).toBeNull();
  });
});

describe("summarizeScopes", () => {
  it("folds read and write of one area into one chip, in plain words", () => {
    expect(summarizeScopes(["collections:read", "collections:write", "storage:read"]).map((s) => s.label)).toEqual(["Records: read and write", "Files: read"]);
  });
  it("names the resource and flags wide grants", () => {
    const [one, all, star] = summarizeScopes(["collections:read:posts", "kv:*", "*"]);
    expect(one).toMatchObject({ label: "Records (posts): read", wide: false, scopes: ["collections:read:posts"] });
    expect(all).toMatchObject({ label: "Key-value: everything", wide: true });
    expect(star).toMatchObject({ label: "Full access", wide: true });
  });
});

describe("the MCP scope", () => {
  it("reads as AI agent access, in the key list and in tooltips", () => {
    expect(MCP_SCOPE).toBe("mcp:*");
    expect(describeScope(MCP_SCOPE)).toBe("AI agents may use this key through the MCP server");
    expect(summarizeScopes(["collections:read", MCP_SCOPE]).map((c) => c.label)).toEqual(["Records: read", "AI agents (MCP)"]);
  });
});
