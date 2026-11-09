import { describe, expect, it } from "vitest";
import { GRAPH_OPERATIONS, VECTOR_OPERATIONS } from ".";
import { specOperations } from "../test/contract";

describe("vector and graph clients vs /api/openapi.json", () => {
  const spec = new Set(specOperations());
  it("only call documented operations", () => {
    expect([...VECTOR_OPERATIONS, ...GRAPH_OPERATIONS].filter((o) => !spec.has(o))).toEqual([]);
  });
  it("cover every documented vector and graph operation", () => {
    const have = new Set<string>([...VECTOR_OPERATIONS, ...GRAPH_OPERATIONS]);
    expect(specOperations().filter((o) => /\/api\/v1\/(vector|graph)\//.test(o) && !have.has(o))).toEqual([]);
  });
  it("documents the 17 vector routes of ADR-0012", () => {
    expect(specOperations().filter((o) => o.includes("/api/v1/vector/"))).toHaveLength(17);
  });
});
