import type {
  EventEnvelope,
  ExecutionLocation,
  RiskClass,
  RoutingMode,
  TaskState,
} from "@tramontana/shared";

import { describeEvent, projectState } from "./activity.js";

/**
 * The activity stream as cards a person reads top to bottom, and the facts a
 * task header shows.
 *
 * Presentation only, like `activity.ts`: every card is derived from durable
 * events, in the order they happened, and nothing is looked up anywhere else.
 * Events that only move the task along — state changes, the worker and the
 * model starting — are the header's to show. Everything else becomes a card,
 * and a kind this build does not know becomes a note rather than disappearing
 * (`CORE_CONTRACTS.md` §2).
 */

/** Where one tool invocation stands, from request to result. */
export type ToolStatus = "requested" | "refused" | "running" | "completed" | "failed";

/** One tool invocation: what was asked for, how it was decided, what came of it. */
export interface ToolCard {
  readonly kind: "tool";
  readonly key: string;
  readonly sequence: number;
  readonly toolId: string;
  readonly riskClass: RiskClass;
  readonly arguments: string;
  readonly status: ToolStatus;
  /** How it was allowed or refused, once that was decided. */
  readonly decision?: string;
  /** What came of it: the result's summary, or the failure. */
  readonly result?: string;
}

/** One routing decision, and what was sent under it. */
export interface RoutingCard {
  readonly kind: "routing";
  readonly key: string;
  readonly sequence: number;
  readonly model: string;
  readonly location: string;
  readonly mode: RoutingMode;
  /** Whether the chosen model is off this device. */
  readonly leavesDevice: boolean;
  readonly reasons: readonly string[];
  /** What was released under this decision, once it was. */
  readonly release?: string;
}

/** Anything else worth reading, as one line. */
export interface DetailCard {
  readonly kind:
    | "context"
    | "message"
    | "checkpoint"
    | "artifact"
    | "note"
    | "completed"
    | "cancelled"
    | "failed";
  readonly key: string;
  readonly sequence: number;
  readonly detail: string;
}

export type ActivityCard = ToolCard | RoutingCard | DetailCard;

/** How an allowed invocation was allowed. */
const GRANT_TEXT = {
  implicitSafeDefault: "allowed without asking",
  once: "allowed once",
  project: "allowed in this project",
} as const;

/** Kinds the header carries, which get no card of their own. */
const HEADER_KINDS: ReadonlySet<string> = new Set([
  "taskCreated",
  "taskStateChanged",
  "workerSelected",
  "workerStarted",
  "workerStopped",
  "modelStarted",
  "configurationResolved",
]);

/** Where a model ran, in words. */
export function describeLocation(location: ExecutionLocation): string {
  if (location.kind === "local") return `this device · ${location.displayName}`;
  const region = location.region === undefined ? "" : ` (${location.region})`;
  return `cloud · ${location.providerId}${region}`;
}

/** Turn a task's events into cards, in the order they happened. */
export function buildCards(events: readonly EventEnvelope[]): ActivityCard[] {
  const cards: ActivityCard[] = [];
  const tools = new Map<string, number>();
  const routes = new Map<string, number>();

  // Folds a later event into its invocation's card. Returns false when the
  // request was never seen, so the event is shown on its own instead.
  const updateTool = (invocationId: string, change: (card: ToolCard) => ToolCard): boolean => {
    const at = tools.get(invocationId);
    const card = at === undefined ? undefined : cards[at];
    if (at === undefined || card?.kind !== "tool") return false;
    cards[at] = change(card);
    return true;
  };

  for (const event of events) {
    const { payload, eventId: key, sequence } = event;
    const detail = (kind: DetailCard["kind"]): void => {
      cards.push({ kind, key, sequence, detail: describeEvent(event).detail });
    };

    switch (payload.kind) {
      case "toolRequested":
        tools.set(payload.invocationId, cards.length);
        cards.push({
          kind: "tool",
          key,
          sequence,
          toolId: payload.toolId,
          riskClass: payload.riskClass,
          arguments: payload.argumentsSummary,
          status: "requested",
        });
        break;
      case "permissionDecided": {
        const how = payload.allowed
          ? payload.grantKind === undefined
            ? "allowed"
            : GRANT_TEXT[payload.grantKind]
          : "refused";
        const folded = updateTool(payload.invocationId, (card) => ({
          ...card,
          decision: `${how} (${payload.reasonCode})`,
          status: payload.allowed ? card.status : "refused",
        }));
        if (!folded) detail("note");
        break;
      }
      case "toolStarted":
        if (!updateTool(payload.invocationId, (card) => ({ ...card, status: "running" }))) {
          detail("note");
        }
        break;
      case "toolCompleted": {
        const result = payload.truncated
          ? `${payload.displaySummary} (truncated)`
          : payload.displaySummary;
        if (
          !updateTool(payload.invocationId, (card) => ({ ...card, status: "completed", result }))
        ) {
          detail("note");
        }
        break;
      }
      case "toolFailed": {
        const result = `${payload.error.code}: ${payload.error.message}`;
        if (!updateTool(payload.invocationId, (card) => ({ ...card, status: "failed", result }))) {
          detail("failed");
        }
        break;
      }
      case "routingDecided": {
        const decision = payload.decision;
        routes.set(decision.decisionId, cards.length);
        cards.push({
          kind: "routing",
          key,
          sequence,
          model: decision.model.modelId,
          location: describeLocation(decision.location),
          mode: decision.mode,
          leavesDevice: decision.requiresDisclosure,
          reasons: decision.reasonCodes,
        });
        break;
      }
      case "contextReleased": {
        const at = routes.get(payload.plan.decisionId);
        const card = at === undefined ? undefined : cards[at];
        const release = describeEvent(event).detail;
        if (at !== undefined && card?.kind === "routing") cards[at] = { ...card, release };
        else detail("note");
        break;
      }
      case "contextPrepared":
        detail("context");
        break;
      case "messageCommitted":
        detail("message");
        break;
      case "checkpointSaved":
        detail("checkpoint");
        break;
      case "artifactRecorded":
        detail("artifact");
        break;
      case "diagnosticRecorded":
        detail("note");
        break;
      case "taskCompleted":
        detail("completed");
        break;
      case "taskCancelled":
        detail("cancelled");
        break;
      case "taskFailed":
        detail("failed");
        break;
      default:
        if (!HEADER_KINDS.has(payload.kind)) detail("note");
    }
  }
  return cards;
}

/** What a task header shows, each fact from the record or absent. */
export interface TaskHeader {
  readonly state: TaskState | undefined;
  /** The worker running it, by its display name. */
  readonly worker: string | undefined;
  /** The model most recently chosen. */
  readonly model: string | undefined;
  /** Where that model runs. */
  readonly location: string | undefined;
  /** The mode the task was started under, or failing that, the last one routed under. */
  readonly mode: RoutingMode | undefined;
  /**
   * Whether anything of this task was released off the device.
   *
   * From authorized releases only: a decision that pointed at the cloud and
   * was then refused sent nothing, and saying otherwise would be as wrong as
   * the reverse.
   */
  readonly leftDevice: boolean;
}

/** The header's facts, from the same events as the cards. */
export function describeHeader(events: readonly EventEnvelope[]): TaskHeader {
  let worker: string | undefined;
  let model: string | undefined;
  let location: string | undefined;
  let createdMode: RoutingMode | undefined;
  let routedMode: RoutingMode | undefined;
  let leftDevice = false;
  for (const { payload } of events) {
    switch (payload.kind) {
      case "taskCreated":
        createdMode = payload.routingMode;
        break;
      case "workerSelected":
        worker = payload.descriptor.displayName;
        break;
      case "routingDecided":
        model = payload.decision.model.modelId;
        location = describeLocation(payload.decision.location);
        routedMode = payload.decision.mode;
        break;
      case "contextReleased":
        leftDevice ||= payload.plan.requiresDisclosure && payload.plan.authorization !== undefined;
        break;
      default:
        break;
    }
  }
  return {
    state: projectState(events),
    worker,
    model,
    location,
    mode: createdMode ?? routedMode,
    leftDevice,
  };
}
