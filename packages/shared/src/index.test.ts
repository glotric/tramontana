import { describe, expect, it } from "vitest";

import schema from "../schema/tramontana-contracts.schema.json";
import { isSchemaCompatible, SCHEMA_VERSION } from "./index.js";

describe("schema compatibility", () => {
  it("accepts a core reporting the mirrored version", () => {
    expect(isSchemaCompatible(SCHEMA_VERSION)).toBe(true);
  });

  it("rejects a core reporting a different version", () => {
    expect(isSchemaCompatible(SCHEMA_VERSION + 1)).toBe(false);
    expect(isSchemaCompatible(SCHEMA_VERSION - 1)).toBe(false);
  });

  it("matches the version stamped into the generated schema by Rust", () => {
    // Nothing at runtime forces the hand-written constant and the Rust one to
    // agree, so the generated artifact is the arbiter.
    expect(SCHEMA_VERSION).toBe(schema["x-schemaVersion"]);
  });
});
