import { describe, expect, it } from "vitest";
import { csv, formatEnv, formatHooks, parseEnv, parseHooks } from "./util";
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
