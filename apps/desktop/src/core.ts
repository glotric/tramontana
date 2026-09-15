import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  ApprovalDecisionKind,
  ApprovalRequest,
  ConversationRecord,
  CoreInfo,
  EffectiveConfiguration,
  ErrorEnvelope,
  EventEnvelope,
  ProjectRecord,
  TaskCheckpoint,
  TaskRecord,
  TaskSnapshot,
  TaskUpdate,
} from "@tramontana/shared";

/**
 * Thin handwritten client over the Tauri command boundary.
 *
 * Transport only: naming, shape, and error normalization. No policy and no
 * domain rules (`docs/CORE_CONTRACTS.md` §14). Every payload type here is
 * generated from the Rust contracts, so a contract change breaks this file at
 * compile time rather than at runtime.
 */

/** Wire shape of the `core_info` response. */
interface CoreInfoPayload {
  readonly schemaVersion: number;
  readonly appVersion: string;
}

/** Versioned project-inspection response from the desktop adapter. */
export interface ProjectInspection {
  readonly schemaVersion: number;
  readonly project: ProjectRecord;
  readonly configuration: EffectiveConfiguration | null;
}

/** Ask the running core for its contract schema and application version. */
export async function fetchCoreInfo(): Promise<CoreInfo> {
  const payload = await invoke<CoreInfoPayload>("core_info");
  return { schemaVersion: payload.schemaVersion, appVersion: payload.appVersion };
}

/** Ask the native folder picker for a root and open it as a durable project. */
export async function openProject(): Promise<ProjectInspection | undefined> {
  return (await invoke<ProjectInspection | null>("open_project")) ?? undefined;
}

/** List recent projects from the active durable profile. */
export async function listProjects(): Promise<readonly ProjectRecord[]> {
  return invoke<ProjectRecord[]>("list_projects");
}

/** Read one project and the latest configuration manifest resolved for it. */
export async function inspectProject(projectId: string): Promise<ProjectInspection> {
  return invoke<ProjectInspection>("inspect_project", { projectId });
}

/** Create a durable project conversation. */
export async function createConversation(
  projectId: string,
  title: string,
): Promise<ConversationRecord> {
  return invoke<ConversationRecord>("create_conversation", { projectId, title });
}

/** List durable conversations in a project. */
export async function listConversations(projectId: string): Promise<readonly ConversationRecord[]> {
  return invoke<ConversationRecord[]>("list_conversations", { projectId });
}

/** List task records, optionally within one conversation. */
export async function listTasks(conversationId?: string): Promise<readonly TaskRecord[]> {
  return invoke<TaskRecord[]>("list_tasks", { conversationId: conversationId ?? null });
}

/** Replay canonical durable events for a task. */
export async function replayTaskEvents(taskId: string): Promise<readonly EventEnvelope[]> {
  return invoke<EventEnvelope[]>("replay_task_events", { taskId });
}

/**
 * Start a task, delivering every update to `onUpdate`.
 *
 * Resolves with the task identifier as soon as the task is registered, not
 * when it finishes — the work continues and reports through the channel.
 */
export async function startTask(
  goal: string,
  onUpdate: (update: TaskUpdate) => void,
  conversationId?: string,
): Promise<string> {
  const channel = new Channel<TaskUpdate>();
  channel.onmessage = onUpdate;
  return invoke<string>("start_task", {
    goal,
    conversationId: conversationId ?? null,
    channel,
  });
}

/** Request cancellation. The task converges to a terminal state on its own. */
export async function cancelTask(taskId: string): Promise<void> {
  return invoke("cancel_task", { taskId });
}

/** Read the current view of a task. */
export async function getTask(taskId: string): Promise<TaskSnapshot> {
  return invoke<TaskSnapshot>("get_task", { taskId });
}

/** Stop a task and write an inspectable checkpoint. */
export async function createCheckpoint(taskId: string): Promise<TaskCheckpoint> {
  return invoke<TaskCheckpoint>("create_checkpoint", { taskId });
}

/** Read the latest checkpoint. Inspection changes nothing. */
export async function latestCheckpoint(taskId: string): Promise<TaskCheckpoint | undefined> {
  return (await invoke<TaskCheckpoint | null>("latest_checkpoint", { taskId })) ?? undefined;
}

/**
 * Resume the exact checkpoint that was inspected.
 *
 * The identifier is required by the command, not defaulted here: resuming
 * "the latest" would let this client act on a checkpoint nobody saw.
 */
export async function resumeTask(taskId: string, checkpointId: string): Promise<void> {
  return invoke("resume_task", { taskId, checkpointId });
}

/** The shell's event for a tool call that needs a person's decision. */
const APPROVAL_REQUESTED = "tramontana://approval-requested";

/**
 * Approval questions this process is still waiting on, oldest first.
 *
 * Asked for on start: the event that raised a card is not replayed, so a
 * question older than this window would otherwise never be seen.
 */
export async function listPendingApprovals(): Promise<readonly ApprovalRequest[]> {
  return invoke<ApprovalRequest[]>("pending_approvals");
}

/**
 * Deliver one decision.
 *
 * The decision travels by identifier. Which answers may be given is the core's
 * to say, and it refuses one it never offered.
 */
export async function resolveApproval(
  approvalId: string,
  decision: ApprovalDecisionKind,
): Promise<void> {
  return invoke("resolve_approval", { approvalId, decision });
}

/** Watch for approval questions. Resolves with the unsubscribe function. */
export async function onApprovalRequested(
  handler: (request: ApprovalRequest) => void,
): Promise<() => void> {
  return listen<ApprovalRequest>(APPROVAL_REQUESTED, (event) => {
    handler(event.payload);
  });
}

/** Discard interrupted work. History is kept and no effect is undone. */
export async function discardCheckpoint(taskId: string): Promise<void> {
  return invoke("discard_checkpoint", { taskId });
}

/**
 * Narrow an unknown rejection to the normalized error the core sends.
 *
 * Tauri rejects with whatever the command returned, so this is the one place
 * that decides whether a failure is a structured `ErrorEnvelope` or something
 * unexpected. Nothing downstream should be inspecting `unknown`.
 */
export function asErrorEnvelope(error: unknown): ErrorEnvelope | undefined {
  if (typeof error !== "object" || error === null) return undefined;
  const candidate = error as Partial<ErrorEnvelope>;
  return typeof candidate.code === "string" && typeof candidate.message === "string"
    ? (candidate as ErrorEnvelope)
    : undefined;
}
