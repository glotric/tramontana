import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import Ajv2020 from "ajv/dist/2020.js";
import { describe, expect, it } from "vitest";
import type { EventEnvelope, TaskState } from "./index.js";

import schema from "../schema/tramontana-contracts.schema.json";

/**
 * Cross-language contract fixtures (`TESTING_STRATEGY.md` §4).
 *
 * Rust produced these streams; TypeScript parses them and validates every event
 * against the committed JSON Schema. This is what proves the two languages
 * genuinely agree, rather than each agreeing with itself.
 */

const fixtures = join(
  dirname(fileURLToPath(import.meta.url)),
  "../../../tests/fixtures/contracts/v1",
);

function readStream(name: string): EventEnvelope[] {
  return readFileSync(join(fixtures, name), "utf8")
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line) => JSON.parse(line) as EventEnvelope);
}

const validate = new Ajv2020({ strict: false }).compile({
  ...schema,
  $ref: "#/$defs/EventEnvelope",
});

describe("contract fixtures produced by Rust", () => {
  it("validates every event against the committed schema", () => {
    for (const name of ["task-completed.ndjson", "task-cancelled.ndjson"]) {
      for (const event of readStream(name)) {
        expect(
          validate(event),
          `${name} seq ${event.sequence}: ${JSON.stringify(validate.errors)}`,
        ).toBe(true);
      }
    }
  });

  it("carries sequences starting at 1 with no gaps", () => {
    for (const name of ["task-completed.ndjson", "task-cancelled.ndjson"]) {
      const sequences = readStream(name).map((event) => event.sequence);
      expect(sequences, name).toEqual(sequences.map((_, index) => index + 1));
    }
  });

  it("ends a completed run in exactly one terminal state", () => {
    const events = readStream("task-completed.ndjson");
    const terminal = events.filter(
      (event) =>
        event.payload.kind === "taskStateChanged" &&
        (["completed", "failed", "cancelled"] as TaskState[]).includes(event.payload.current),
    );
    expect(terminal).toHaveLength(1);
    expect(events.at(-1)?.payload.kind).toBe("taskCompleted");
  });

  it("records the cancellation path rather than skipping to the end", () => {
    // Cancelling is durable evidence that the user asked, as distinct from a
    // task that merely stopped.
    const events = readStream("task-cancelled.ndjson");
    const states = events.flatMap((event) =>
      event.payload.kind === "taskStateChanged" ? [event.payload.current] : [],
    );
    expect(states).toContain("cancelling");
    expect(states.at(-1)).toBe("cancelled");
    expect(events.at(-1)?.payload.kind).toBe("taskCancelled");
  });

  it("never carries a secret-visibility event to the interface", () => {
    for (const name of ["task-completed.ndjson", "task-cancelled.ndjson"]) {
      for (const event of readStream(name)) {
        expect(event.visibility, name).not.toBe("secret");
      }
    }
  });
});
