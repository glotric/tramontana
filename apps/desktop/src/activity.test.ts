import { describe, expect, it } from "vitest";
import type { EventEnvelope, EventPayload } from "@tramontana/shared";

import { describeEvent, isTerminal, projectState } from "./activity.js";

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

describe("activity rendering", () => {
  it("describes each event kind the skeleton emits", () => {
    expect(describeEvent(event(1, { kind: "taskCreated", goalSummary: "say hello" })).detail).toBe(
      "goal: say hello",
    );

    expect(
      describeEvent(
        event(2, {
          kind: "taskStateChanged",
          previous: "queued",
          current: "preparing",
          reasonCode: "task.advanced",
        }),
      ).detail,
    ).toBe("queued → preparing (task.advanced)");

    expect(
      describeEvent(
        event(3, {
          kind: "toolRequested",
          invocationId: "i1",
          toolId: "fake.echo",
          argumentsSummary: "1 argument(s): value",
          riskClass: "read",
        }),
      ).detail,
    ).toBe("fake.echo (read) — 1 argument(s): value");
  });

  it("says a release left the device, and on whose authority", () => {
    // A person scanning the activity list must not have to open anything to
    // learn that their project data went to a third party.
    const detail = describeEvent(
      event(4, {
        kind: "contextReleased",
        plan: {
          schemaVersion: 1,
          releaseId: "release-1",
          taskId: "task-1",
          requestId: "request-1",
          decisionId: "decision-1",
          contextManifestId: "context-1",
          destination: { providerId: "openai", modelId: "gpt-oss" },
          location: { kind: "cloud", providerId: "openai" },
          requiresDisclosure: true,
          reasonCodes: ["cloudPreferred"],
          messageIds: ["message-1"],
          released: [
            {
              category: "conversation",
              dataClass: "project",
              sourceCount: 1,
              byteCount: 9,
            },
          ],
          dataClasses: ["project"],
          totalBytes: 9,
          redactions: [],
          exclusions: [
            {
              sourceId: "AGENTS.md",
              category: "instruction",
              reasonCode: "release.notTransmitted",
            },
          ],
          toolsOffered: [],
          authorization: {
            reasonCode: "release.approvedOnce",
            approvalId: "approval-1",
            mode: "cloudPreferred",
          },
          createdAt: "2026-01-01T00:00:00.000Z",
        },
      }),
    ).detail;

    expect(detail).toBe(
      "9 byte(s) of project context left the device to gpt-oss (openai), 1 withheld — release.approvedOnce",
    );
  });

  it("shows an unknown event kind rather than dropping or crashing on it", () => {
    // A newer core may emit kinds this build has never seen. The contract is to
    // display them as unsupported, not to fail (`CORE_CONTRACTS.md` §2).
    const future = event(9, { kind: "quantumEntangled" } as unknown as EventPayload);
    expect(describeEvent(future).detail).toBe("unsupported event kind");
    expect(describeEvent(future).sequence).toBe(9);
  });

  it("projects task state from the event stream rather than tracking it apart", () => {
    expect(projectState([])).toBeUndefined();
    expect(projectState([event(1, { kind: "taskCreated", goalSummary: "x" })])).toBe("queued");
    expect(
      projectState([
        event(1, { kind: "taskCreated", goalSummary: "x" }),
        event(2, {
          kind: "taskStateChanged",
          previous: "queued",
          current: "running",
          reasonCode: "worker.started",
        }),
        event(3, {
          kind: "taskStateChanged",
          previous: "running",
          current: "completed",
          reasonCode: "worker.completed",
        }),
      ]),
    ).toBe("completed");
  });

  it("agrees with the contract about which states are terminal", () => {
    for (const state of ["completed", "failed", "cancelled"] as const) {
      expect(isTerminal(state)).toBe(true);
    }
    for (const state of ["queued", "running", "cancelling", "interrupted"] as const) {
      expect(isTerminal(state)).toBe(false);
    }
  });
});
