import { describe, expect, it } from "vitest";
import { compileIssues, csv, formatEnv, formatHooks, parseEnv, parseHooks } from "./util";
import { wasmToBase64 } from "../../api";

describe("function form helpers", () => {
  it("parses and formats env", () => {
    expect(parseEnv("A=1\n\nB=x=y")).toEqual({ A: "1", B: "x=y" });
    expect(formatEnv({ A: "1", B: "2" })).toBe("A=1\nB=2");
    expect(() => parseEnv("nope")).toThrow(/line 1/);
  });
  it("parses and formats hooks", () => {
    expect(parseHooks("orders: after_create, after_update")).toEqual([{ collection: "orders", events: ["after_create", "after_update"] }]);
    expect(formatHooks(parseHooks("a: before_delete"))).toBe("a: before_delete");
    expect(() => parseHooks("orders")).toThrow(/line 1/);
    expect(() => parseHooks("orders: later")).toThrow(/unknown event later/);
  });
  it("splits csv and encodes large modules", () => {
    expect(csv(" a, b ,,c")).toEqual(["a", "b", "c"]);
    const big = new Uint8Array(100_000).fill(7);
    expect(atob(wasmToBase64(big))).toHaveLength(100_000);
  });
});

describe("compileIssues", () => {
  const err = (message: string, details?: unknown, code = "function_compile_failed") => ({ code, message, body: { error: { code, message, ...(details ? { details } : {}) } } });
  it("reads line and column from the details, a list of them, or the message", () => {
    expect(compileIssues(err("bad", { line: 3, column: 14 }))).toEqual([{ line: 3, column: 14, message: "bad" }]);
    expect(compileIssues(err("2 errors", { errors: [{ line: 1, column: 2, message: "a" }, { line: 4, col: 5, message: "b" }] }))).toEqual([
      { line: 1, column: 2, message: "a" }, { line: 4, column: 5, message: "b" },
    ]);
    expect(compileIssues(err("unexpected token at 7:9"))).toEqual([{ line: 7, column: 9, message: "unexpected token at 7:9" }]);
    expect(compileIssues(err("syntax error at line 2, column 8"))).toEqual([{ line: 2, column: 8, message: "syntax error at line 2, column 8" }]);
  });
  it("ignores other errors and errors without a position", () => {
    expect(compileIssues(err("nope", undefined, "forbidden"))).toEqual([]);
    expect(compileIssues(err("bad source"))).toEqual([]);
    expect(compileIssues(new Error("x"))).toEqual([]);
  });
});
