import { describe, expect, it } from "vitest";
import type { FeatureSetting, ServerFeature } from "../../api";
import { groupSettings } from "./settingUtil";

const setting = (key: string): FeatureSetting => ({ key, label: key, help: "", type: "string", value: null, default: null, source: "default", editable: true, restart_required: false, secret: false });

describe("groupSettings", () => {
  it("lists the unnamed group first, then PostgreSQL before extensions and anything else (FB-155: PostgreSQL is the default)", () => {
    const f = { id: "database", settings: ["database.sqlite.synchronous", "database.postgres.extensions.vector", "database.postgres.pool_size", "database.path"].map(setting) } as unknown as ServerFeature;
    expect(groupSettings(f).map((g) => g.title)).toEqual([null, "PostgreSQL", "PostgreSQL extensions", "Sqlite"]);
  });
});
