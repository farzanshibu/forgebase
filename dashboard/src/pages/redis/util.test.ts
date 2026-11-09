import { describe, expect, it } from "vitest";
import { binFrom, binText } from "../../api";
import { formatReply, formatTtl, parseFields, tokenize } from "./util";

describe("redis util", () => {
  it("tokenizes console lines", () => {
    expect(tokenize("SET a b")).toEqual(["SET", "a", "b"]);
    expect(tokenize('SET "a b" \'c d\' ""')).toEqual(["SET", "a b", "c d", ""]);
    expect(tokenize('SET k "say \\"hi\\"\\n"')).toEqual(["SET", "k", 'say "hi"\n']);
    expect(tokenize("   ")).toEqual([]);
    expect(() => tokenize('GET "x')).toThrow("Unbalanced quote");
  });
  it("formats replies like redis-cli", () => {
    expect(formatReply(null)).toBe("(nil)");
    expect(formatReply(3)).toBe("(integer) 3");
    expect(formatReply("OK")).toBe('"OK"');
    expect(formatReply([])).toBe("(empty array)");
    expect(formatReply(["a", ["b"]])).toBe('1) "a"\n2) 1) "b"');
    expect(formatReply({ base64: "AAE=" })).toBe("base64:AAE=");
  });
  it("formats TTLs", () => {
    expect(formatTtl(null)).toBe("no expiry");
    expect(formatTtl(500)).toBe("500 ms");
    expect(formatTtl(90_000)).toBe("90 s");
    expect(formatTtl(600_000)).toBe("10 min");
    expect(formatTtl(7_200_000)).toBe("2 h");
    expect(formatTtl(5 * 86_400_000)).toBe("5 d");
  });
  it("parses name=value lines", () => {
    expect(parseFields("a=1\n\nb=x=y")).toEqual({ a: "1", b: "x=y" });
    expect(() => parseFields("oops")).toThrow("Line 1");
    expect(() => parseFields("")).toThrow("at least one");
  });
  it("round-trips binary values", () => {
    expect(binText({ base64: "AAE=" })).toBe("base64:AAE=");
    expect(binFrom("base64:AAE=")).toEqual({ base64: "AAE=" });
    expect(binFrom("plain")).toBe("plain");
    expect(binText(null)).toBe("");
  });
});
