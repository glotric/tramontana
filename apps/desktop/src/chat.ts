import type { EventEnvelope, TaskState } from "@tramontana/shared";

import { projectState } from "./activity.js";

/**
 * A chat, read from the record (ADR 0057). Each turn is one task: what the
 * person asked, and what came back. Nothing here decides anything; it only
 * reads the canonical events the core already wrote.
 */

/** One exchange in a chat. */
export interface ChatTurn {
  readonly taskId: string;
  readonly goal: string;
  readonly events: readonly EventEnvelope[];
}

/** How a turn stands, in words the chat can show. */
export type TurnOutcome =
  | { readonly kind: "answered"; readonly text: string }
  | { readonly kind: "working" }
  | { readonly kind: "waiting" }
  | { readonly kind: "failed"; readonly message: string }
  | { readonly kind: "stopped" };

/** The text of the turn's last committed assistant message, if any. */
export function answerOf(events: readonly EventEnvelope[]): string | undefined {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const payload = events[index]?.payload;
    if (
      payload?.kind === "messageCommitted" &&
      payload.message.role === "assistant" &&
      payload.message.status === "committed"
    ) {
      const text = payload.message.parts
        .map((part) => (part.kind === "text" ? part.text : ""))
        .join("\n")
        .trim();
      if (text !== "") return text;
    }
  }
  return undefined;
}

const WAITING: readonly TaskState[] = ["interrupted"];

/** What the chat says about a turn. */
export function outcomeOf(events: readonly EventEnvelope[]): TurnOutcome {
  const answer = answerOf(events);
  const state = projectState(events);
  // The terminal event itself, not only the state it projects: it carries
  // the reason, and it is the record's last word on how the turn ended.
  const ended = events.findLast(
    (event) =>
      event.payload.kind === "taskFailed" ||
      event.payload.kind === "taskCancelled" ||
      event.payload.kind === "taskCompleted",
  )?.payload;
  if (ended?.kind === "taskFailed" || state === "failed") {
    return {
      kind: "failed",
      message: ended?.kind === "taskFailed" ? ended.error.message : "This answer failed.",
    };
  }
  if (ended?.kind === "taskCancelled" || state === "cancelled") {
    return answer === undefined ? { kind: "stopped" } : { kind: "answered", text: answer };
  }
  if (answer !== undefined) return { kind: "answered", text: answer };
  if (state !== undefined && WAITING.includes(state)) return { kind: "waiting" };
  return { kind: "working" };
}

/**
 * Add an event to the turn it belongs to, once. A turn the window has not
 * seen yet is started from the event, and takes its goal from the record's
 * own `taskCreated`.
 */
export function withEvent(
  turns: readonly ChatTurn[],
  event: EventEnvelope,
  goal?: string,
): readonly ChatTurn[] {
  const index = turns.findIndex((turn) => turn.taskId === event.taskId);
  if (index === -1) {
    const created = event.payload.kind === "taskCreated" ? event.payload.goalSummary : undefined;
    return [...turns, { taskId: event.taskId, goal: goal ?? created ?? "", events: [event] }];
  }
  const turn = turns[index];
  if (turn === undefined || turn.events.some((seen) => seen.sequence === event.sequence)) {
    return turns;
  }
  const events = [...turn.events, event].sort((a, b) => a.sequence - b.sequence);
  const named =
    turn.goal === "" && event.payload.kind === "taskCreated"
      ? event.payload.goalSummary
      : turn.goal;
  return turns.map((each, at) => (at === index ? { ...turn, goal: named, events } : each));
}

/** A chat's title, from the first thing asked in it. */
export function titleFrom(goal: string): string {
  const line = goal.trim().split(/\r?\n/)[0]?.trim() ?? "";
  if (line === "") return "New chat";
  return line.length > 60 ? `${line.slice(0, 57).trimEnd()}…` : line;
}
