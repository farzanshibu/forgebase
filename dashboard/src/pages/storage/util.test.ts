import { describe, expect, it } from "vitest";
import { baseName, crumbs, parseBytes, parseTypes } from "./util";
import { encKey } from "../../api";

describe("storage util", () => {
  it("splits keys and prefixes", () => {
    expect(baseName("a/b/c.txt", "a/b/")).toBe("c.txt");
    expect(baseName("a/b/", "a/")).toBe("b");
    expect(crumbs("a/b/")).toEqual([{ label: "a", prefix: "a/" }, { label: "b", prefix: "a/b/" }]);
    expect(crumbs("")).toEqual([]);
  });
  it("parses sizes and types", () => {
    expect(parseBytes("")).toBeNull();
    expect(parseBytes("10 MiB")).toBe(10485760);
    expect(parseBytes("1.5 kib")).toBe(1536);
    expect(parseBytes("2048")).toBe(2048);
    expect(() => parseBytes("lots")).toThrow(/Enter a size/);
    expect(parseTypes("")).toBeNull();
    expect(parseTypes("image/png, text/plain\napplication/pdf")).toEqual(["image/png", "text/plain", "application/pdf"]);
  });
  it("escapes key segments but keeps the slashes", () => {
    expect(encKey("a b/ü/c#d")).toBe("a%20b/%C3%BC/c%23d");
  });
});
