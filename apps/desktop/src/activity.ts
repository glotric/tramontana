import type { EventEnvelope, EventPayload, TaskState } from "@tramontana/shared";

/**
 * Turning contract events into something a person can read.
 *
 * Presentation only. If a line needs a fact the event does not carry, the fix
 * is a richer event, not a lookup here — the CLI renderer and this module must
 * be able to say the same things from the same durable record.
 */

/** A rendered activity line. */
export interface ActivityLine {
  readonly sequence: number;
  readonly kind: string;
  readonly detail: string;
}

/** Describe one durable event in a single line. */
export function describeEvent(event: EventEnvelope): ActivityLine {
  return {
    sequence: event.sequence,
    kind: event.kind,
    detail: describePayload(event.payload),
  };
}

function describePayload(payload: EventPayload): string {
  switch (payload.kind) {
    case "taskCreated":
      return `goal: ${payload.goalSummary}`;
    case "taskStateChanged":
      return `${payload.previous} → ${payload.current} (${payload.reasonCode})`;
    case "workerSelected":
      return `${payload.displayName} [${payload.agentId}]`;
    case "modelStarted":
      return `model ${payload.modelId}`;
    case "configurationResolved":
      return `manifest ${payload.configuration.manifestHash}`;
    case "messageCommitted":
      return `${payload.message.role} message, ${payload.message.parts.length} part(s)`;
    case "toolRequested":
      return `${payload.toolId} (${payload.riskClass}) — ${payload.argumentsSummary}`;
    case "toolCompleted":
      return payload.truncated ? `${payload.displaySummary} (truncated)` : payload.displaySummary;
    case "taskCompleted":
      return payload.summary;
    case "taskFailed":
      return `${payload.error.code}: ${payload.error.message}`;
    case "taskCancelled":
      return `cancelled by ${payload.origin}`;
    default:
      // A newer core may emit kinds this build has never seen. Showing the
      // kind and carrying on is the contract (`CORE_CONTRACTS.md` §2).
      return "unsupported event kind";
  }
}

/** Whether a state can never be left. */
export function isTerminal(state: TaskState): boolean {
  return state === "completed" || state === "failed" || state === "cancelled";
}

/**
 * The task state implied by the events seen so far.
 *
 * Derived from the stream rather than tracked separately, so the interface
 * cannot drift from the durable record.
 */
export function projectState(events: readonly EventEnvelope[]): TaskState | undefined {
  let state: TaskState | undefined;
  for (const event of events) {
    if (event.payload.kind === "taskStateChanged") {
      state = event.payload.current;
    } else if (event.payload.kind === "taskCreated") {
      state ??= "queued";
    }
  }
  return state;
}
