import { describe, expect, it } from "vitest";
import { buildScopes, describeScope, emptyChoice, parseScopes, readExample } from "./scopes";

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
