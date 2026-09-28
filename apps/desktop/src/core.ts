import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  ApprovalDecisionKind,
  NewSchedule,
  PersonalDetails,
  PersonalProfile,
  Schedule,
  ScheduleView,
  SearchResults,
  TranscriptOptions,
  ApprovalRequest,
  ConversationRecord,
  CoreInfo,
  EffectiveConfiguration,
  ErrorEnvelope,
  EventEnvelope,
  ExportManifest,
  ProjectGrant,
  ProjectRecord,
  RoutingMode,
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

/**
 * Create a durable conversation in a project, or a chat when no project is
 * named: a conversation with no files, whose tasks are asked to answer
 * (ADR 0057).
 */
export async function createConversation(
  projectId: string | undefined,
  title: string,
): Promise<ConversationRecord> {
  return invoke<ConversationRecord>("create_conversation", {
    projectId: projectId ?? null,
    title,
  });
}

/** List durable conversations in a project, or the chats when none is named. */
export async function listConversations(
  projectId: string | undefined,
): Promise<readonly ConversationRecord[]> {
  return invoke<ConversationRecord[]>("list_conversations", { projectId: projectId ?? null });
}

/** List task records, optionally within one conversation. */
export async function listTasks(conversationId?: string): Promise<readonly TaskRecord[]> {
  return invoke<TaskRecord[]>("list_tasks", { conversationId: conversationId ?? null });
}

/** A project's working tree as Git sees it. */
export interface ProjectChanges {
  readonly schemaVersion: number;
  /** Git's status, including files it does not track yet. */
  readonly status: string;
  /** Git's per-file summary and patch for tracked files. */
  readonly diff: string;
  /** Whether either read was cut short by its output limit. */
  readonly truncated: boolean;
}

/**
 * Read a project's Git status and diff for the person.
 *
 * Changes nothing and belongs to no task (ADR 0032).
 */
export async function projectChanges(projectId: string): Promise<ProjectChanges> {
  return invoke<ProjectChanges>("project_changes", { projectId });
}

/** The standing permissions a project holds, so a person can see what they allowed. */
export async function listProjectGrants(projectId: string): Promise<readonly ProjectGrant[]> {
  return invoke<ProjectGrant[]>("list_project_grants", { projectId });
}

/** Revoke one. A later request it would have covered is no longer covered by it. */
export async function revokeProjectGrant(grantId: string): Promise<void> {
  return invoke("revoke_project_grant", { grantId });
}

/** Whether a credential is stored, never its value. */
export interface CredentialStatus {
  /** The key it is filed under, such as `openai.apiKey`. */
  readonly key: string;
  /** Whether one is stored; absent when the store could not answer. */
  readonly configured?: boolean;
  /** Why the store could not answer, when it could not. */
  readonly unavailableReason?: string;
}

/** One configured model endpoint. */
export interface ConfiguredEndpoint {
  readonly endpointId: string;
  readonly displayName: string;
  readonly kind: "openAiCompatible" | "anthropic" | "llamaServer" | "mlxServer";
  readonly modelId: string;
  /** Whether work sent here leaves the device. Derived, never declared. */
  readonly leavesTheDevice: boolean;
  readonly enabled: boolean;
  /** Tool-call support the person verified. Unknown is off. */
  readonly supportsToolCalls: boolean;
  readonly credential?: CredentialStatus;
  /** A runtime Tramontana starts: its files and whether they changed. */
  readonly runtime?: RuntimeFiles;
}

/** Whether a runtime's file is still what was recorded (ADR 0043). */
export type FileStatus = "confirmed" | "held" | "unreadable" | "notRecorded";

/** A supervised runtime's files, as recorded when it was added or confirmed. */
export interface RuntimeFiles {
  /** The executable's file name only. */
  readonly executable: string;
  /** `held` means it changed, and will not be started until confirmed. */
  readonly executableStatus: FileStatus;
  /** The model's size: the least memory it needs. */
  readonly modelBytes?: number;
  readonly recordedAt?: string;
}

/** Whether a configured endpoint came up when Tramontana opened. */
export interface ModelEndpointStatus {
  readonly endpointId: string;
  readonly models: readonly string[];
  readonly leavesTheDevice: boolean;
  readonly available: boolean;
  readonly unavailableReason?: string;
  /**
   * For a runtime Tramontana started, the accelerators it reported; empty
   * when it reported none, so it may be running on the processor alone.
   */
  readonly accelerators?: readonly string[];
}

/** Every configured endpoint, with whether its credential is stored. */
export async function listConfiguredEndpoints(): Promise<readonly ConfiguredEndpoint[]> {
  return invoke<ConfiguredEndpoint[]>("list_configured_endpoints");
}

/** The endpoints that came up when Tramontana opened, and those that did not. */
export async function listModelEndpoints(): Promise<readonly ModelEndpointStatus[]> {
  return invoke<ModelEndpointStatus[]>("list_model_endpoints");
}

/** What every new endpoint is given. */
interface NewEndpoint {
  /** The name routing records use. No spaces. */
  readonly endpointId: string;
  /** What to call it in the window; the name when absent. */
  readonly displayName?: string;
  readonly modelId: string;
}

/**
 * Configure a server speaking the OpenAI chat-completions format.
 *
 * Resolves with whether work sent to it leaves the device. Takes effect when
 * Tramontana restarts.
 */
export async function addOpenAiEndpoint(
  endpoint: NewEndpoint & { readonly baseUrl: string; readonly credentialKey?: string },
): Promise<boolean> {
  return invoke<boolean>("add_openai_endpoint", {
    endpointId: endpoint.endpointId,
    displayName: endpoint.displayName ?? null,
    baseUrl: endpoint.baseUrl,
    modelId: endpoint.modelId,
    credentialKey: endpoint.credentialKey ?? null,
  });
}

/** Configure Anthropic's Messages API, which needs a credential. */
export async function addAnthropicEndpoint(
  endpoint: NewEndpoint & { readonly baseUrl: string; readonly credentialKey: string },
): Promise<boolean> {
  return invoke<boolean>("add_anthropic_endpoint", {
    endpointId: endpoint.endpointId,
    displayName: endpoint.displayName ?? null,
    baseUrl: endpoint.baseUrl,
    modelId: endpoint.modelId,
    credentialKey: endpoint.credentialKey,
  });
}

/**
 * Configure a llama-server Tramontana starts. The shell asks the native picker
 * for the executable and the model file; resolves `undefined` when either is
 * cancelled, and nothing is saved.
 */
export async function addLlamaEndpoint(endpoint: NewEndpoint): Promise<boolean | undefined> {
  return (
    (await invoke<boolean | null>("add_llama_endpoint", {
      endpointId: endpoint.endpointId,
      displayName: endpoint.displayName ?? null,
      modelId: endpoint.modelId,
    })) ?? undefined
  );
}

/**
 * Configure an `mlx_lm.server` Tramontana starts (ADR 0010). The shell asks
 * the native picker for the executable and the model folder; resolves
 * `undefined` when either is cancelled, and nothing is saved. Runs only on
 * Apple Silicon; elsewhere it is saved and shown as unavailable.
 */
export async function addMlxEndpoint(endpoint: NewEndpoint): Promise<boolean | undefined> {
  return (
    (await invoke<boolean | null>("add_mlx_endpoint", {
      endpointId: endpoint.endpointId,
      displayName: endpoint.displayName ?? null,
      modelId: endpoint.modelId,
    })) ?? undefined
  );
}

/** Record a runtime's files as they are now. Takes effect on restart. */
export async function trustEndpoint(endpointId: string): Promise<void> {
  return invoke("trust_endpoint", { endpointId });
}

/** Whether a runtime's model is still what was recorded. Reads all of it. */
export async function checkEndpointModel(endpointId: string): Promise<FileStatus> {
  return invoke<FileStatus>("check_endpoint_model", { endpointId });
}

/** Turn an endpoint on or off. Takes effect when Tramontana restarts. */
export async function setEndpointEnabled(endpointId: string, enabled: boolean): Promise<void> {
  return invoke("set_endpoint_enabled", { endpointId, enabled });
}

/** Record whether the person verified an endpoint's tool-call support. */
export async function setEndpointToolCalls(endpointId: string, supported: boolean): Promise<void> {
  return invoke("set_endpoint_tool_calls", { endpointId, supported });
}

/** Remove an endpoint. Its credential stays in the store. */
export async function removeEndpoint(endpointId: string): Promise<void> {
  return invoke("remove_endpoint", { endpointId });
}

/** Store a credential. Resolves with its length, never the value. */
export async function storeCredential(key: string, value: string): Promise<number> {
  return invoke<number>("store_credential", { key, value });
}

/** Remove a credential from the store. Resolves with whether one was there. */
export async function forgetCredential(key: string): Promise<boolean> {
  return invoke<boolean>("forget_credential", { key });
}

/** Restart Tramontana, so endpoints configured since it opened are used. */
export async function restartApplication(): Promise<void> {
  return invoke("restart_application");
}

/**
 * Whether "Restart now" can work. Not under the development server, which
 * stops when the application exits and leaves a restarted window blank.
 */
export async function restartSupported(): Promise<boolean> {
  return invoke<boolean>("restart_supported");
}

/** How many records of each kind a bundle holds, or an import wrote. */
export interface BundleCounts {
  readonly projects: number;
  readonly conversations: number;
  readonly tasks: number;
  readonly events: number;
  readonly checkpoints: number;
  readonly configurations: number;
  /** Opaque worker state that is refused rather than restored. */
  readonly quarantined: number;
}

/** A bundle that was written: where, and its table of contents. */
export interface ExportReport {
  readonly schemaVersion: number;
  readonly directory: string;
  readonly manifest: ExportManifest;
}

/** A bundle the person picked and inspected, waiting for them to confirm. */
export interface StagedBundle {
  /** What the import confirms, instead of a path. */
  readonly stagingId: string;
  readonly inspection: {
    readonly schemaVersion: number;
    readonly manifest: ExportManifest;
    readonly contains: BundleCounts;
  };
}

/** What an import wrote. */
export interface ImportReport {
  readonly schemaVersion: number;
  readonly imported: BundleCounts;
  /** Always false: nothing in a bundle is run. */
  readonly executedAnything: boolean;
}

/**
 * Write a bundle for one task or one project into a folder the person picks.
 *
 * Resolves `undefined` when the picker is cancelled. The folder never comes
 * from here: the shell asks the native picker.
 */
export async function exportBundle(
  scope: { readonly taskId: string } | { readonly projectId: string },
  options: { readonly includeAbsolutePaths: boolean; readonly includeRawToolOutput: boolean },
): Promise<ExportReport | undefined> {
  return (
    (await invoke<ExportReport | null>("export_bundle", {
      taskId: "taskId" in scope ? scope.taskId : null,
      projectId: "projectId" in scope ? scope.projectId : null,
      includeAbsolutePaths: options.includeAbsolutePaths,
      includeRawToolOutput: options.includeRawToolOutput,
    })) ?? undefined
  );
}

/** Pick a bundle folder and say what it holds. Writes nothing. */
export async function inspectBundle(): Promise<StagedBundle | undefined> {
  return (await invoke<StagedBundle | null>("inspect_bundle")) ?? undefined;
}

/** Import the bundle that was inspected, by the identifier it was given. */
export async function importStagedBundle(stagingId: string): Promise<ImportReport> {
  return invoke<ImportReport>("import_staged_bundle", { stagingId });
}

/** Replay canonical durable events for a task. */
export async function replayTaskEvents(taskId: string): Promise<readonly EventEnvelope[]> {
  return invoke<EventEnvelope[]>("replay_task_events", { taskId });
}

/**
 * What a person chose for one task before starting it (ADR 0031).
 *
 * Choices, not permissions: `writes` and `recipes` only let the model ask,
 * and each write or recipe run is still put to the person as an approval.
 */
export interface TaskOptions {
  /** Let the model ask to make local, reversible file changes. */
  readonly writes: boolean;
  /** Let the model ask to run the project's configured recipes. */
  readonly recipes: boolean;
  /**
   * Let the model be offered the tools of the profile's Apps that say they
   * only read (ADR 0036). Every call is still put to the person.
   */
  readonly apps: boolean;
  /**
   * Let the model browse the web in a window of its own (ADR 0047). The
   * first visit to each site is put to the person, and every form
   * submission.
   */
  readonly browse: boolean;
  /** Where this task's models may run. Kept if the task is resumed. */
  readonly routingMode: RoutingMode;
  /**
   * The worker to run it, from {@link listWorkers} (ADR 0048). Absent is
   * the built-in worker on the model the routing mode chooses.
   */
  readonly worker?: string;
  /**
   * The project's skills this task uses, by declared id, from
   * {@link listSkills} (ADR 0053). Their text is given to the model, in the
   * order named. A skill grants nothing.
   */
  readonly skills?: readonly string[];
  /**
   * Files picked with {@link chooseAttachments} for this message, by token
   * (ADR 0058). Their text is given to the model before the goal.
   */
  readonly attachments?: readonly string[];
}

/**
 * Start a task, delivering every update to `onUpdate`.
 *
 * Resolves with the task identifier as soon as the task is registered, not
 * when it finishes — the work continues and reports through the channel.
 * Without `options` the task runs read-only under the profile's mode.
 */
export async function startTask(
  goal: string,
  onUpdate: (update: TaskUpdate) => void,
  conversationId?: string,
  options?: TaskOptions,
): Promise<string> {
  const channel = new Channel<TaskUpdate>();
  channel.onmessage = onUpdate;
  return invoke<string>("start_task", {
    goal,
    conversationId: conversationId ?? null,
    choices:
      options === undefined
        ? null
        : {
            admission: {
              writes: options.writes,
              recipes: options.recipes,
              apps: options.apps,
              browse: options.browse,
            },
            routingMode: options.routingMode,
            worker: options.worker ?? null,
            skills: options.skills === undefined ? null : [...options.skills],
            attachments: options.attachments === undefined ? null : [...options.attachments],
          },
    channel,
  });
}

/**
 * Follow a task that is already running, from the last event this window has.
 *
 * What a reloaded window does: the task went on without it, and this delivers
 * every event after `afterSequence`, then the live stream, to `onUpdate`.
 */
export async function watchTask(
  taskId: string,
  afterSequence: number,
  onUpdate: (update: TaskUpdate) => void,
): Promise<void> {
  const channel = new Channel<TaskUpdate>();
  channel.onmessage = onUpdate;
  return invoke("watch_task", { taskId, afterSequence, channel });
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

/** A worker a stopped task can be handed to (ADR 0048). */
export interface WorkerView {
  /** The name a handoff uses. */
  readonly agentId: string;
  /** What a person calls it. */
  readonly displayName: string;
  /** Whether its model runs off this device; absent when the mode chooses. */
  readonly cloud?: boolean;
}

/** The workers a stopped task can be handed to. */
export async function listWorkers(): Promise<readonly WorkerView[]> {
  return invoke<WorkerView[]>("list_workers");
}

/** What a stopped task can be handed to, in the order to offer it (ADR 0055). */
export interface HandoffOffer {
  /** The workers, best first; the first is the suggestion. */
  readonly workers: readonly WorkerView[];
  /**
   * Whether the task stopped because the work was beyond its model, so the
   * suggestion is to escalate rather than to try another model here.
   */
  readonly escalating: boolean;
}

/**
 * What this interrupted task can be handed to, ordered by the core.
 *
 * The worker that already has it is left out, a Local Only task is offered
 * nothing off this device, and a task whose model ran out is offered the
 * cloud first. Nothing is handed anywhere without {@link handOffTask}.
 */
export async function handoffOptions(taskId: string): Promise<HandoffOffer> {
  return invoke<HandoffOffer>("handoff_options", { taskId });
}

/** A skill a task can be given (ADR 0053). */
export interface SkillView {
  /** The declared id a task names to select it. */
  readonly id: string;
  /** What a person calls it: its declared name, or its file name. */
  readonly displayName: string;
  /** Its one-line description, when it declares one. */
  readonly description?: string;
}

/**
 * The skills a project offers, from the configuration last resolved for it.
 *
 * Offering is not selecting: a task uses only the ones it names.
 */
export async function listSkills(projectId: string): Promise<readonly SkillView[]> {
  return invoke<SkillView[]>("list_skills", { projectId });
}

/**
 * Which of a project's skills look relevant to `goal`, by declared id
 * (ADR 0054).
 *
 * Suggesting is not selecting: these are marked and shown first, and a task
 * still uses only the skills the person ticks. The rule is the core's, so the
 * CLI suggests the same ones.
 */
export async function suggestSkills(projectId: string, goal: string): Promise<readonly string[]> {
  return invoke<string[]>("suggest_skills", { projectId, goal });
}

/**
 * Hand the checkpoint on screen to another worker, which continues the task
 * from its record with a budget of its own. Like resuming, it names the
 * checkpoint the person inspected.
 */
export async function handOffTask(
  taskId: string,
  checkpointId: string,
  agentId: string,
): Promise<void> {
  return invoke("hand_off_task", { taskId, checkpointId, agentId });
}

/** Where an App stands in this window (ADR 0040). */
export type AppStatus =
  | { readonly kind: "running" }
  | { readonly kind: "stopped"; readonly reason: string }
  | { readonly kind: "didNotStart"; readonly reason: string }
  | { readonly kind: "off" }
  | { readonly kind: "startsOnRestart" }
  | { readonly kind: "stopsOnRestart" };

/** One tool an App registered. */
export interface AppToolView {
  readonly toolId: string;
  /** The App's own description. Presentation, never policy. */
  readonly description: string;
  /** Whether a task that admits Apps may offer it to a model. */
  readonly offered: boolean;
}

/** One configured App, with nothing it is given shown by value. */
export interface AppView {
  readonly appId: string;
  readonly displayName: string;
  readonly enabled: boolean;
  readonly status: AppStatus;
  /** The program's file name only; empty for a remote App. */
  readonly program: string;
  /** A remote App's address (ADR 0050); absent for one that runs here. */
  readonly address?: string;
  /** Names of the variables and secrets it is given; never their values. */
  readonly variables: readonly string[];
  readonly secrets: readonly string[];
  readonly protocolVersion?: string;
  readonly tools: readonly AppToolView[];
  readonly refused: readonly { readonly name: string; readonly reason: string }[];
  /** The last of what it wrote to its error stream, already redacted. */
  readonly diagnostics: string;
}

/** How one call to an App's tool was decided, and how it ended. */
export interface AppDecision {
  readonly taskId: string;
  readonly goal: string;
  readonly toolId: string;
  readonly allowed: boolean;
  readonly reasonCode: string;
  readonly grantKind?: "implicitSafeDefault" | "once" | "project" | "site";
  readonly outcome?: "completed" | "failed";
  readonly decidedAt: string;
}

/** An App's decisions, newest first, and whether the search stopped early. */
export interface AppHistory {
  readonly decisions: readonly AppDecision[];
  readonly partial: boolean;
}

/** Every configured App, and how each is doing in this window. */
export async function listApps(): Promise<readonly AppView[]> {
  return invoke<AppView[]>("list_apps");
}

/** Turn an App on or off. Takes effect when Tramontana restarts. */
export async function setAppEnabled(appId: string, enabled: boolean): Promise<void> {
  return invoke("set_app_enabled", { appId, enabled });
}

/** What was decided about an App's tools, from the durable record. */
export async function appPermissionHistory(appId: string): Promise<AppHistory> {
  return invoke<AppHistory>("app_permission_history", { appId });
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

/** A file picked for the next message, held by the desktop until it is sent. */
export interface StagedAttachment {
  readonly token: string;
  readonly name: string;
  readonly byteCount: number;
}

/**
 * Pick files to attach to the next message (ADR 0058). The desktop reads
 * and holds them; the window only ever sees their names and sizes. Empty when
 * the person cancels.
 */
export async function chooseAttachments(): Promise<readonly StagedAttachment[]> {
  return invoke<StagedAttachment[]>("choose_attachments");
}

/** Stop holding a picked file the person removed before sending. */
export async function discardAttachment(token: string): Promise<void> {
  return invoke("discard_attachment", { token });
}

/** Give a conversation a new title (ADR 0059). */
export async function renameConversation(
  conversationId: string,
  title: string,
): Promise<ConversationRecord> {
  return invoke<ConversationRecord>("rename_conversation", { conversationId, title });
}

/**
 * Delete a conversation and every task, event and checkpoint recorded under
 * it (ADR 0059). Ask the person first: this cannot be undone.
 */
export async function deleteConversation(conversationId: string): Promise<void> {
  return invoke("delete_conversation", { conversationId });
}

/** Search conversation titles, what was asked in them, and projects (ADR 0059). */
export async function search(query: string): Promise<SearchResults> {
  return invoke<SearchResults>("search", { query });
}

/** The transcript a share would save, to read first (ADR 0060). */
export async function previewTranscript(
  conversationId: string,
  options: TranscriptOptions,
): Promise<string> {
  return invoke<string>("preview_transcript", { conversationId, options });
}

/**
 * Save a conversation's transcript where the person chooses (ADR 0060).
 * Resolves with where it was written, or `undefined` when they cancel.
 */
export async function saveTranscript(
  conversationId: string,
  options: TranscriptOptions,
): Promise<string | undefined> {
  return (await invoke<string | null>("save_transcript", { conversationId, options })) ?? undefined;
}

/** The person's profile and memory notes (ADR 0061). */
export async function personalProfile(): Promise<PersonalProfile> {
  return invoke<PersonalProfile>("personal_profile");
}

/** Change what chats are told about the person. */
export async function updatePersonal(details: PersonalDetails): Promise<PersonalProfile> {
  return invoke<PersonalProfile>("update_personal", { details });
}

/** Add a note for chats to remember. */
export async function remember(text: string): Promise<PersonalProfile> {
  return invoke<PersonalProfile>("remember", { text });
}

/** Forget one note. */
export async function forget(noteId: string): Promise<PersonalProfile> {
  return invoke<PersonalProfile>("forget", { noteId });
}

/** Every schedule, with when it runs next (ADR 0063). */
export async function listSchedules(): Promise<readonly ScheduleView[]> {
  return invoke<ScheduleView[]>("list_schedules");
}

/** Create a schedule and the conversation its runs are turns in. */
export async function createSchedule(schedule: NewSchedule): Promise<ScheduleView> {
  return invoke<ScheduleView>("create_schedule", { schedule });
}

/** Turn a schedule on or off. */
export async function setScheduleEnabled(scheduleId: string, enabled: boolean): Promise<Schedule> {
  return invoke<Schedule>("set_schedule_enabled", { scheduleId, enabled });
}

/** Delete a schedule; its conversation is kept. */
export async function deleteSchedule(scheduleId: string): Promise<void> {
  return invoke("delete_schedule", { scheduleId });
}

/** Run a schedule now. */
export async function runScheduleNow(scheduleId: string): Promise<string> {
  return invoke<string>("run_schedule_now", { scheduleId });
}
