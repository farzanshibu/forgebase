import { describe, expect, it } from "vitest";
import { ApiError } from "./api";
import { ERROR_WORDS, explain, queryPosition } from "./errors";

const err = (code: string, message: string, details?: unknown) =>
  new ApiError({ status: 400, code, message, requestId: "r1", body: { error: { code, message, request_id: "r1", ...(details ? { details } : {}) } } });

describe("openCypher error positions (ADR-0034)", () => {
  it("reads the line and column from the message the server ends with", () => {
    expect(queryPosition(err("cypher_unsupported", "WITH is not supported by the portable openCypher engine (line 3, column 14)"))).toEqual({ line: 3, column: 14 });
    expect(queryPosition(err("cypher_too_complex", "a path may have at most 10 hops (line 1, column 22)"))).toEqual({ line: 1, column: 22 });
    expect(queryPosition(err("cypher_invalid", "expected RETURN (line 2, column 1)"))).toEqual({ line: 2, column: 1 });
  });
  it("prefers numbers in details when the server sends them", () => {
    expect(queryPosition(err("cypher_unsupported", "no position in the text", { line: 4, column: 9 }))).toEqual({ line: 4, column: 9 });
  });
  it("has none for other codes, plain errors, or a message without a position", () => {
    expect(queryPosition(err("invalid_filter", "bad (line 1, column 2)"))).toBeUndefined();
    expect(queryPosition(err("cypher_error", "label Foo does not exist"))).toBeUndefined();
    expect(queryPosition(new Error("(line 1, column 2)"))).toBeUndefined();
    expect(queryPosition("x")).toBeUndefined();
  });
  it("carries the position into the explained error", () => {
    const e = explain(err("cypher_unsupported", "OPTIONAL MATCH is not supported by the portable openCypher engine (line 1, column 1)"));
    expect(e.position).toEqual({ line: 1, column: 1 });
    expect(e.title).toMatch(/built-in engine does not support/);
    expect(e.next).toMatch(/OPTIONAL MATCH/);
    expect(explain(err("cypher_error", "boom")).position).toBeUndefined();
  });
  it("explains cypher_too_complex in plain words", () => {
    const e = explain(err("cypher_too_complex", "too deep"));
    expect(e.title).toBe("That query is too big or too deep for the built-in engine");
    expect(e.next).toMatch(/at most 10 hops/);
  });
});

describe("the retired fuzzy and index-kind codes", () => {
  it("no longer say PostgreSQL is needed: current servers run both everywhere", () => {
    for (const code of ["fuzzy_unsupported", "index_kind_unsupported"] as const) {
      const words = `${ERROR_WORDS[code].title} ${ERROR_WORDS[code].next}`;
      expect(words).not.toMatch(/move the (project|database) to PostgreSQL|needs PostgreSQL/i);
      expect(words).toMatch(/Update the server/);
      expect(words).toMatch(/ADR-0034/);
    }
  });
  it("still explains an old server's answer", () => {
    const e = explain(err("index_kind_unsupported", "trigram needs pg_trgm"));
    expect(e.title).toBe("This server version cannot build that kind of index");
    expect(e.next).toMatch(/pick btree/);
  });
});
