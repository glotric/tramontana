import { describe, expect, it } from "vitest";
import type { EventEnvelope, EventPayload } from "@tramontana/shared";

import { commandRuns, commandUnderway } from "./terminal.js";

let sequence = 0;
function event(payload: EventPayload): EventEnvelope {
  sequence += 1;
  return {
    schemaVersion: 1,
    eventId: `e${sequence}`,
    taskId: "t",
    sequence,
    kind: payload.kind,
    audience: "user",
    occurredAt: "2026-10-09T10:00:00Z",
    correlationId: "c",
    payload,
  } as unknown as EventEnvelope;
}

const requested = (id: string, tool = "builtin.recipe.run"): EventEnvelope =>
  event({
    kind: "toolRequested",
    invocationId: id,
    toolId: tool,
    argumentsSummary: "recipe test",
    riskClass: "dangerous",
  });

const result = (id: string, exitCode: number, stdout: string): EventEnvelope =>
  event({
    kind: "messageCommitted",
    message: {
      messageId: `m-${id}`,
      role: "tool",
      createdAt: "2026-10-09T10:00:01Z",
      parts: [
        {
          kind: "toolResult",
          invocationId: id,
          content: { exitCode, stdout, stderr: "", truncated: false },
          truncated: false,
        },
      ],
    },
  } as unknown as EventPayload);

describe("commandRuns", () => {
  it("follows a command from its request to its output", () => {
    const waiting = commandRuns([requested("a")]);
    expect(waiting).toEqual([{ invocationId: "a", command: "recipe test", status: "waiting" }]);
    expect(commandUnderway(waiting)).toBe(true);

    const events = [
      requested("a"),
      event({
        kind: "toolStarted",
        invocationId: "a",
        toolId: "builtin.recipe.run",
      } as EventPayload),
      result("a", 0, "Ran 2 tests\nOK\n"),
      event({
        kind: "toolCompleted",
        invocationId: "a",
        displaySummary: "Recipe `test` passed",
      } as EventPayload),
    ];
    const [run] = commandRuns(events);
    expect(run?.status).toBe("done");
    expect(run?.stdout).toBe("Ran 2 tests\nOK\n");
    expect(run?.exitCode).toBe(0);
    expect(commandUnderway(commandRuns(events))).toBe(false);
  });

  it("calls a non-zero exit a failure, whatever order its events came in", () => {
    const [run] = commandRuns([
      requested("b"),
      event({
        kind: "toolCompleted",
        invocationId: "b",
        displaySummary: "Recipe `test` failed (exit 1)",
      } as EventPayload),
      result("b", 1, "FAILED (failures=2)"),
    ]);
    expect(run?.status).toBe("failed");
    expect(run?.note).toBe("Recipe `test` failed (exit 1)");
  });

  it("says when a command was refused, and ignores tools that are not commands", () => {
    const runs = commandRuns([
      requested("c"),
      event({ kind: "permissionDecided", invocationId: "c", allowed: false } as EventPayload),
      requested("d", "builtin.fs.read"),
    ]);
    expect(runs).toHaveLength(1);
    expect(runs[0]?.status).toBe("refused");
  });
});
