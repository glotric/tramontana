import { describe, expect, it } from "vitest";
import type {
  ContextReleasePlan,
  EventEnvelope,
  EventPayload,
  RoutingDecision,
} from "@tramontana/shared";

import { buildCards, describeHeader } from "./cards.js";

function event(sequence: number, payload: EventPayload): EventEnvelope {
  return {
    schemaVersion: 1,
    eventId: `event-${sequence}`,
    taskId: "task-1",
    sequence,
    occurredAt: "2026-01-01T00:00:00.000Z",
    kind: payload.kind as EventEnvelope["kind"],
    visibility: "user",
    correlationId: "c1",
    payload,
  };
}

const requested = (sequence: number, invocationId: string, toolId: string): EventEnvelope =>
  event(sequence, {
    kind: "toolRequested",
    invocationId,
    toolId,
    argumentsSummary: "1 argument(s): path",
    riskClass: "modify",
  });

const decided = (
  sequence: number,
  invocationId: string,
  allowed: boolean,
  reasonCode: string,
): EventEnvelope =>
  event(sequence, {
    kind: "permissionDecided",
    invocationId,
    capability: "fs.write",
    allowed,
    reasonCode,
    ...(allowed ? { grantKind: "once" as const } : {}),
    origin: { kind: "worker", agentId: "builtin.reference" },
    scope: { releasesData: false },
  });

function decision(decisionId: string, cloud: boolean): RoutingDecision {
  return {
    schemaVersion: 1,
    decisionId,
    mode: cloud ? "cloudPreferred" : "localOnly",
    model: cloud
      ? { providerId: "openai", modelId: "gpt-oss" }
      : { providerId: "local", modelId: "qwen2.5-7b-instruct" },
    location: cloud
      ? { kind: "cloud", providerId: "openai" }
      : { kind: "local", targetId: "llama", displayName: "llama.cpp" },
    availabilitySnapshotId: "snapshot-1",
    reasonCodes: [cloud ? "cloudPreferred" : "localOnly"],
    fallbackOrder: [],
    contextManifestId: "context-1",
    requiresDisclosure: cloud,
  };
}

function release(decisionId: string, authorized: boolean): ContextReleasePlan {
  return {
    schemaVersion: 1,
    releaseId: `release-${decisionId}`,
    taskId: "task-1",
    requestId: "request-1",
    decisionId,
    contextManifestId: "context-1",
    destination: { providerId: "openai", modelId: "gpt-oss" },
    location: { kind: "cloud", providerId: "openai" },
    requiresDisclosure: true,
    reasonCodes: ["cloudPreferred"],
    messageIds: ["message-1"],
    released: [{ category: "conversation", dataClass: "project", sourceCount: 1, byteCount: 9 }],
    dataClasses: ["project"],
    totalBytes: 9,
    redactions: [],
    exclusions: [],
    toolsOffered: [],
    ...(authorized
      ? {
          authorization: {
            reasonCode: "release.approvedOnce",
            approvalId: "approval-1",
            mode: "cloudPreferred" as const,
          },
        }
      : {}),
    createdAt: "2026-01-01T00:00:00.000Z",
  };
}

describe("activity cards", () => {
  it("follows one tool call from request to result on a single card", () => {
    const cards = buildCards([
      requested(1, "i1", "builtin.fs.patch"),
      decided(2, "i1", true, "permission.approved"),
      event(3, {
        kind: "toolStarted",
        invocationId: "i1",
        toolId: "builtin.fs.patch",
        replaySafety: "safe",
      }),
      event(4, {
        kind: "toolCompleted",
        invocationId: "i1",
        displaySummary: "patched greeting.txt",
        sideEffect: "localReversible",
        truncated: false,
      }),
    ]);

    expect(cards).toEqual([
      {
        kind: "tool",
        key: "event-1",
        sequence: 1,
        toolId: "builtin.fs.patch",
        riskClass: "modify",
        arguments: "1 argument(s): path",
        status: "completed",
        decision: "allowed once (permission.approved)",
        result: "patched greeting.txt",
      },
    ]);
  });

  it("says when a tool was refused, and what a failed one reported", () => {
    const cards = buildCards([
      requested(1, "i1", "builtin.fs.patch"),
      decided(2, "i1", false, "permission.denied"),
      requested(3, "i2", "builtin.fs.create"),
      decided(4, "i2", true, "permission.approved"),
      event(5, {
        kind: "toolFailed",
        invocationId: "i2",
        error: {
          schemaVersion: 1,
          code: "invalidRequest",
          message: "the content hash did not match",
          retryable: false,
          correlationId: "c1",
        },
      }),
    ]);

    expect(
      cards.map((card) => card.kind === "tool" && [card.status, card.decision, card.result]),
    ).toEqual([
      ["refused", "refused (permission.denied)", undefined],
      [
        "failed",
        "allowed once (permission.approved)",
        "invalidRequest: the content hash did not match",
      ],
    ]);
  });

  it("puts a release on the decision it was made under, and says it left the device", () => {
    const cards = buildCards([
      event(1, { kind: "routingDecided", decision: decision("d1", true) }),
      event(2, { kind: "contextReleased", plan: release("d1", true) }),
    ]);

    expect(cards).toHaveLength(1);
    const [card] = cards;
    expect(card).toMatchObject({
      kind: "routing",
      model: "gpt-oss",
      location: "cloud · openai",
      leavesDevice: true,
    });
    expect(card?.kind === "routing" && card.release).toMatch(/left the device to gpt-oss/);
  });

  it("leaves progress to the header and keeps what it does not know", () => {
    const cards = buildCards([
      event(1, { kind: "taskCreated", goalSummary: "Say hello" }),
      event(2, {
        kind: "taskStateChanged",
        previous: "queued",
        current: "running",
        reasonCode: "worker.started",
      }),
      event(3, {
        kind: "diagnosticRecorded",
        code: "reference.callsNotRun",
        summary: "1 call not run",
      }),
      event(4, { kind: "quantumEntangled" } as unknown as EventPayload),
      event(5, { kind: "taskCompleted", summary: "Done" }),
    ]);

    expect(cards.map((card) => [card.sequence, card.kind])).toEqual([
      [3, "note"],
      [4, "note"],
      [5, "completed"],
    ]);
  });

  it("shows what the model said, and the CLI's line for any other message", () => {
    const message = (sequence: number, role: "assistant" | "user", text: string): EventEnvelope =>
      event(sequence, {
        kind: "messageCommitted",
        message: {
          schemaVersion: 1,
          messageId: `message-${sequence}`,
          role,
          status: "committed",
          parts: [{ kind: "text", text }],
          createdAt: "2026-01-01T00:00:00.000Z",
        },
      });

    const cards = buildCards([
      message(1, "assistant", "I read the file."),
      message(2, "user", "Say hello"),
    ]);
    expect(cards.map((card) => card.kind === "message" && card.detail)).toEqual([
      "I read the file.",
      "user message, 1 part(s)",
    ]);
  });

  it("shows a tool result whose request it never saw, rather than dropping it", () => {
    const cards = buildCards([
      event(1, {
        kind: "toolFailed",
        invocationId: "unseen",
        error: {
          schemaVersion: 1,
          code: "timeout",
          message: "took too long",
          retryable: true,
          correlationId: "c1",
        },
      }),
    ]);
    expect(cards).toMatchObject([{ kind: "failed", sequence: 1 }]);
  });
});

describe("task header", () => {
  it("names the worker, the model, where it ran and the mode the task started under", () => {
    const header = describeHeader([
      event(1, { kind: "taskCreated", goalSummary: "Say hello", routingMode: "localOnly" }),
      event(2, { kind: "routingDecided", decision: decision("d1", false) }),
    ]);

    expect(header).toEqual({
      state: "queued",
      goal: "Say hello",
      worker: undefined,
      model: "qwen2.5-7b-instruct",
      location: "this device · llama.cpp",
      mode: "localOnly",
      leftDevice: false,
    });

    // Before any routing, the header still says what the person chose.
    expect(
      describeHeader([
        event(1, { kind: "taskCreated", goalSummary: "Say hello", routingMode: "localOnly" }),
      ]).mode,
    ).toBe("localOnly");
  });

  it("says the task left the device only when a release off it was authorized", () => {
    const proposed = describeHeader([
      event(1, { kind: "routingDecided", decision: decision("d1", true) }),
      event(2, { kind: "contextReleased", plan: release("d1", false) }),
    ]);
    expect(proposed.leftDevice).toBe(false);
    // A task recorded before the mode was takes the one it was routed under.
    expect(proposed.mode).toBe("cloudPreferred");

    const released = describeHeader([
      event(1, { kind: "routingDecided", decision: decision("d1", true) }),
      event(2, { kind: "contextReleased", plan: release("d1", true) }),
    ]);
    expect(released.leftDevice).toBe(true);
  });
});
