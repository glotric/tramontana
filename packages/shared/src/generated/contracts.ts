/**
 * DO NOT EDIT. Generated from crates/core/src/contracts by `pnpm contracts:generate`.
 *
 * The Rust definitions are the source of truth; `pnpm contracts:check` fails
 * when this file is stale.
 */

/**
 * A worker a provider offers.
 */
export interface AgentDescriptor {
  /**
   * Provider-scoped worker identifier.
   */
  agentId: string;
  /**
   * Provider that offers it.
   */
  agentProviderId: string;
  /**
   * How well it can be stopped and resumed.
   */
  checkpointSupport: "none" | "sameWorker" | "portable";
  /**
   * Human-readable name.
   */
  displayName: string;
  /**
   * How it is integrated.
   */
  integrationMode: "embedded" | "acp" | "customProcess";
  /**
   * Whether it may run under Local Only.
   *
   * Computed by the core from the fields above, never taken from the
   * worker's own claim (`CORE_CONTRACTS.md` §7.2).
   */
  localOnlyEligible: boolean;
  /**
   * Who chooses its model.
   */
  modelControl: "tramontanaBrokered" | "workerManaged";
  /**
   * What it can do on the network.
   */
  networkControl: "denied" | "brokered" | "constrained" | "unobservable";
  /**
   * How it reaches tools.
   */
  toolControl: "tramontanaBrokered" | "directConstrained" | "directUnconstrained";
  /**
   * Why it is unavailable, when it is.
   */
  unavailableReason?: string;
}

/**
 * Who requested a cancellation.
 */
export type CancellationOrigin = "user" | "system";

/**
 * What an authorization is limited to.
 *
 * `SECURITY_AND_PERMISSIONS.md` §3.2. A scope binds the project and, for
 * filesystem work, the canonical roots it may touch. Authorizations are
 * non-transferable: a grant naming one project authorizes nothing in another.
 */
export interface CapabilityScope {
  /**
   * The project this is limited to. Absent means a projectless task, which
   * has no filesystem access at all.
   */
  projectId?: ProjectId | null;
  /**
   * Whether the operation sends project-derived data off the device.
   *
   * A separate dimension from risk: "a read action that sends content to a
   * cloud endpoint is not treated as a harmless local read"
   * (`SECURITY_AND_PERMISSIONS.md` §3.1).
   */
  releasesData: boolean;
  /**
   * Canonical roots the operation may touch.
   */
  roots?: string[];
}

/**
 * A saved checkpoint of a task.
 */
export type CheckpointId = string;

/**
 * How well a worker can be stopped and resumed.
 */
export type CheckpointSupport = "none" | "sameWorker" | "portable";

/**
 * The worker a checkpoint was taken against.
 */
export interface CheckpointWorker {
  descriptor: AgentDescriptor;
  /**
   * Opaque continuation reference, when the worker offered one.
   *
   * Adapter data, never the record: a handoff to a different worker ignores
   * it entirely (`PROJECT_FORMAT.md` §4).
   */
  sessionStateRef?: string;
}

/**
 * Something noticed while loading a source.
 *
 * `CONFIGURATION.md` §5: an invalid or duplicate definition disables the
 * affected item and shows a validation error. It never crashes startup, and a
 * broken file in one project must not make the application unusable.
 */
export interface ConfigurationDiagnostic {
  /**
   * Human-readable explanation.
   */
  message: string;
  /**
   * Project-relative path of the file concerned.
   */
  relativePath: string;
  /**
   * How serious it is.
   */
  severity: "warning" | "error";
}

/**
 * What kind of instruction file a source is.
 */
export type ConfigurationKind =
  | "rootAgents"
  | "scopedAgents"
  | "claude"
  | "projectOverview"
  | "rule"
  | "skill"
  | "agentProfile"
  | "workflow"
  | "unsupported";

/**
 * One loaded instruction file.
 */
export interface ConfigurationSource {
  /**
   * Whether this enabled source participates in this resolved manifest.
   *
   * Nested instructions can be out of scope, while skills, agents, and
   * workflows must be explicitly selected. Keeping this separate from
   * `enabled` makes the effective-configuration view honest about both.
   */
  active: boolean;
  /**
   * Size in bytes.
   */
  byteCount: number;
  /**
   * SHA-256 of the file's bytes, for reproducibility.
   */
  contentHash: string;
  /**
   * Declared `id` from front matter, when present.
   */
  declaredId?: string;
  /**
   * Declared `name` from front matter, when present.
   */
  declaredName?: string;
  /**
   * Whether the item is active. Front matter may set `enabled: false`, and
   * a validation error also disables it.
   */
  enabled: boolean;
  /**
   * What kind of file it is.
   */
  kind:
    | "rootAgents"
    | "scopedAgents"
    | "claude"
    | "projectOverview"
    | "rule"
    | "skill"
    | "agentProfile"
    | "workflow"
    | "unsupported";
  /**
   * Precedence rank; lower wins.
   */
  precedence: number;
  /**
   * Author-defined rule priority. Higher values apply first within the
   * native rule layer.
   */
  priority?: number;
  /**
   * Project-relative path. Never absolute: an absolute path names a machine.
   */
  relativePath: string;
  /**
   * Capabilities requested by a native definition.
   *
   * These are informational. Loading Markdown never grants a capability.
   */
  requestedCapabilities?: string[];
  /**
   * Project-relative scopes declared by the source.
   *
   * A nested `AGENTS.md` contributes its parent directory. Native rules
   * contribute their `applies_to` patterns. An empty list means the whole
   * project.
   */
  scopes?: string[];
  /**
   * Explicit selection order for skills, agents, and workflows.
   */
  selectionOrder?: number;
  /**
   * Stable project-scoped key for this source (currently its relative path).
   */
  sourceId: string;
  /**
   * Front matter keys this build does not recognize.
   *
   * Preserved rather than dropped, so a file authored for a newer version
   * survives a round trip through an older one (`CONFIGURATION.md` §5).
   */
  unknownKeys?: string[];
}

/**
 * A conversation within a project, or a projectless conversation.
 */
export type ConversationId = string;

/**
 * A conversation, optionally scoped to a project.
 */
export interface ConversationRecord {
  /**
   * When it was archived, if it has been.
   */
  archivedAt?: Timestamp | null;
  /**
   * Stable identifier.
   */
  conversationId: string;
  /**
   * An instant, serialized as a UTC RFC 3339 string.
   */
  createdAt: string;
  /**
   * Project this belongs to. Absent means projectless, which carries no
   * implicit filesystem access (`PHASE_1_ACCEPTANCE_CRITERIA.md` §8).
   */
  projectId?: ProjectId | null;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * Display title.
   */
  title: string;
}

/**
 * A correlation identifier shared by everything caused by one request.
 */
export type CorrelationId = string;

/**
 * Sensitivity of a piece of content, used to decide what may leave the device.
 */
export type DataClass = "public" | "project" | "sensitive" | "secret";

/**
 * Why a source was rejected or flagged.
 */
export type DiagnosticSeverity = "warning" | "error";

/**
 * Everything loaded for a project, in precedence order.
 */
export interface EffectiveConfiguration {
  /**
   * Problems found while loading.
   */
  diagnostics: ConfigurationDiagnostic[];
  /**
   * Hash over the ordered source hashes.
   *
   * One value that changes whenever the effective configuration changes, so
   * a task can record what it started with in a single field
   * (`CONFIGURATION.md` §11).
   */
  manifestHash: string;
  /**
   * When this was resolved.
   */
  resolvedAt: string;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * Sources, strongest precedence first.
   */
  sources: ConfigurationSource[];
}

/**
 * Stable error categories.
 *
 * Adapters choose the closest category rather than inventing one; a new
 * variant is a contract change (`CORE_CONTRACTS.md` §15).
 */
export type ErrorCode =
  | "invalidRequest"
  | "notFound"
  | "conflict"
  | "permissionDenied"
  | "approvalRequired"
  | "policyBlocked"
  | "capabilityUnsupported"
  | "providerAuthentication"
  | "providerRateLimited"
  | "providerUnavailable"
  | "workerUnavailable"
  | "toolFailed"
  | "timeout"
  | "cancelled"
  | "limitExceeded"
  | "protocolError"
  | "persistenceError"
  | "migrationRequired"
  | "interruptedUnsafe"
  | "internal";

/**
 * A safe, user-presentable error.
 */
export interface ErrorEnvelope {
  /**
   * Stable category.
   */
  code:
    | "invalidRequest"
    | "notFound"
    | "conflict"
    | "permissionDenied"
    | "approvalRequired"
    | "policyBlocked"
    | "capabilityUnsupported"
    | "providerAuthentication"
    | "providerRateLimited"
    | "providerUnavailable"
    | "workerUnavailable"
    | "toolFailed"
    | "timeout"
    | "cancelled"
    | "limitExceeded"
    | "protocolError"
    | "persistenceError"
    | "migrationRequired"
    | "interruptedUnsafe"
    | "internal";
  /**
   * Correlates this failure with the request and events around it.
   */
  correlationId: string;
  /**
   * Reference to fuller local diagnostics, which are redacted separately.
   */
  diagnosticRef?: string;
  /**
   * Safe, user-presentable message. Never carries secrets or content.
   */
  message: string;
  /**
   * Hint for how long to wait before retrying, when the source supplies one.
   */
  retryAfterMs?: number;
  /**
   * Whether retrying the identical request could succeed.
   */
  retryable: boolean;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
}

/**
 * A durable event.
 */
export interface EventEnvelope {
  /**
   * The command, event, invocation, or approval that caused this one.
   */
  causationId?: string;
  /**
   * Conversation, when the task has one.
   */
  conversationId?: ConversationId | null;
  /**
   * Shared by everything caused by one originating request.
   */
  correlationId: string;
  /**
   * Unique identifier for this event.
   */
  eventId: string;
  /**
   * The event kind, duplicated outside the payload so a reader can route on
   * it without parsing a body it may not understand.
   */
  kind:
    | "taskCreated"
    | "taskStateChanged"
    | "configurationResolved"
    | "contextPrepared"
    | "routingDecided"
    | "workerSelected"
    | "workerStarted"
    | "modelStarted"
    | "messageCommitted"
    | "toolRequested"
    | "approvalRequested"
    | "approvalResolved"
    | "toolStarted"
    | "toolCompleted"
    | "toolFailed"
    | "checkpointSaved"
    | "artifactRecorded"
    | "workerStopped"
    | "taskCompleted"
    | "taskFailed"
    | "taskCancelled"
    | "diagnosticRecorded";
  /**
   * When the event happened.
   */
  occurredAt: string;
  /**
   * The body.
   */
  payload:
    | {
        /**
         * Conversation this task belongs to, when it has one.
         */
        conversationId?: ConversationId | null;
        /**
         * One-line, secret-free summary of what was asked for.
         */
        goalSummary: string;
        kind: "taskCreated";
      }
    | {
        /**
         * State after the move.
         */
        current:
          | "queued"
          | "preparing"
          | "routing"
          | "startingWorker"
          | "running"
          | "awaitingApproval"
          | "checkpointing"
          | "cancelling"
          | "interrupted"
          | "completed"
          | "failed"
          | "cancelled";
        kind: "taskStateChanged";
        /**
         * State before the move.
         */
        previous:
          | "queued"
          | "preparing"
          | "routing"
          | "startingWorker"
          | "running"
          | "awaitingApproval"
          | "checkpointing"
          | "cancelling"
          | "interrupted"
          | "completed"
          | "failed"
          | "cancelled";
        /**
         * Stable, machine-readable reason.
         */
        reasonCode: string;
      }
    | {
        configuration: EffectiveConfiguration;
        kind: "configurationResolved";
      }
    | {
        descriptor: AgentDescriptor;
        kind: "workerSelected";
      }
    | {
        kind: "modelStarted";
        /**
         * Namespaced model identifier.
         */
        modelId: string;
        /**
         * Correlates the ephemeral stream frames with this durable event.
         */
        streamId: string;
      }
    | {
        kind: "messageCommitted";
        message: NormalizedMessage;
      }
    | {
        /**
         * Short, safe description of what was asked for.
         */
        argumentsSummary: string;
        /**
         * Identifies this invocation across its request and result.
         */
        invocationId: string;
        kind: "toolRequested";
        /**
         * How much damage the tool could do.
         */
        riskClass: "read" | "modify" | "dangerous";
        /**
         * Namespaced tool identifier.
         */
        toolId: string;
      }
    | {
        /**
         * Whether it may proceed.
         */
        allowed: boolean;
        /**
         * What was asked for.
         */
        capability: string;
        /**
         * How the authorization was obtained, when it was allowed.
         */
        grantKind?: GrantKind | null;
        /**
         * The invocation the decision covers.
         */
        invocationId: string;
        kind: "permissionDecided";
        /**
         * Who asked.
         */
        origin:
          | {
              kind: "user";
            }
          | {
              /**
               * Namespaced worker identifier.
               */
              agentId: string;
              kind: "worker";
            }
          | {
              kind: "workflow";
              /**
               * Workflow identifier.
               */
              workflowId: string;
            };
        /**
         * Stable reason, matching `ports::reason`.
         */
        reasonCode: string;
        scope: CapabilityScope;
      }
    | {
        /**
         * The invocation.
         */
        invocationId: string;
        kind: "toolStarted";
        /**
         * The tool.
         */
        toolId: string;
      }
    | {
        error: ErrorEnvelope;
        /**
         * The invocation.
         */
        invocationId: string;
        kind: "toolFailed";
      }
    | {
        /**
         * Short, safe description of the result.
         */
        displaySummary: string;
        /**
         * The invocation this closes.
         */
        invocationId: string;
        kind: "toolCompleted";
        /**
         * What the invocation actually changed.
         */
        sideEffect: "none" | "localReversible" | "localIrreversible" | "external";
        /**
         * Whether output was cut short by a limit.
         */
        truncated: boolean;
      }
    | {
        /**
         * The checkpoint.
         */
        checkpointId: string;
        /**
         * How many steps it records as already done.
         */
        completedStepCount: number;
        /**
         * The durable sequence it was taken at.
         */
        eventSequence: number;
        kind: "checkpointSaved";
        /**
         * What was in flight, when anything was.
         */
        pendingAction?: PendingAction | null;
        resume: ResumeEligibility;
      }
    | {
        kind: "taskCompleted";
        /**
         * Secret-free summary of the outcome.
         */
        summary: string;
      }
    | {
        error: ErrorEnvelope;
        kind: "taskFailed";
      }
    | {
        kind: "taskCancelled";
        /**
         * Who or what asked for cancellation.
         */
        origin: "user" | "system";
      }
    | {
        kind: "unsupported";
      };
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * Position in the task's stream. Starts at 1, increases by exactly one.
   */
  sequence: number;
  /**
   * A unit of work with its own event stream and exactly one terminal state.
   */
  taskId: string;
  /**
   * Who this event is for.
   */
  visibility: "user" | "diagnostic" | "secret";
}

/**
 * A single durable event.
 */
export type EventId = string;

/**
 * The body of a durable event.
 *
 * Tagged by `kind` so the discriminator is one field in both Rust and
 * TypeScript. M1 defines the payloads the walking skeleton emits; the rest
 * arrive with the milestones that produce them.
 */
export type EventPayload =
  | {
      /**
       * Conversation this task belongs to, when it has one.
       */
      conversationId?: ConversationId | null;
      /**
       * One-line, secret-free summary of what was asked for.
       */
      goalSummary: string;
      kind: "taskCreated";
    }
  | {
      /**
       * State after the move.
       */
      current:
        | "queued"
        | "preparing"
        | "routing"
        | "startingWorker"
        | "running"
        | "awaitingApproval"
        | "checkpointing"
        | "cancelling"
        | "interrupted"
        | "completed"
        | "failed"
        | "cancelled";
      kind: "taskStateChanged";
      /**
       * State before the move.
       */
      previous:
        | "queued"
        | "preparing"
        | "routing"
        | "startingWorker"
        | "running"
        | "awaitingApproval"
        | "checkpointing"
        | "cancelling"
        | "interrupted"
        | "completed"
        | "failed"
        | "cancelled";
      /**
       * Stable, machine-readable reason.
       */
      reasonCode: string;
    }
  | {
      configuration: EffectiveConfiguration;
      kind: "configurationResolved";
    }
  | {
      descriptor: AgentDescriptor;
      kind: "workerSelected";
    }
  | {
      kind: "modelStarted";
      /**
       * Namespaced model identifier.
       */
      modelId: string;
      /**
       * Correlates the ephemeral stream frames with this durable event.
       */
      streamId: string;
    }
  | {
      kind: "messageCommitted";
      message: NormalizedMessage;
    }
  | {
      /**
       * Short, safe description of what was asked for.
       */
      argumentsSummary: string;
      /**
       * Identifies this invocation across its request and result.
       */
      invocationId: string;
      kind: "toolRequested";
      /**
       * How much damage the tool could do.
       */
      riskClass: "read" | "modify" | "dangerous";
      /**
       * Namespaced tool identifier.
       */
      toolId: string;
    }
  | {
      /**
       * Whether it may proceed.
       */
      allowed: boolean;
      /**
       * What was asked for.
       */
      capability: string;
      /**
       * How the authorization was obtained, when it was allowed.
       */
      grantKind?: GrantKind | null;
      /**
       * The invocation the decision covers.
       */
      invocationId: string;
      kind: "permissionDecided";
      /**
       * Who asked.
       */
      origin:
        | {
            kind: "user";
          }
        | {
            /**
             * Namespaced worker identifier.
             */
            agentId: string;
            kind: "worker";
          }
        | {
            kind: "workflow";
            /**
             * Workflow identifier.
             */
            workflowId: string;
          };
      /**
       * Stable reason, matching `ports::reason`.
       */
      reasonCode: string;
      scope: CapabilityScope;
    }
  | {
      /**
       * The invocation.
       */
      invocationId: string;
      kind: "toolStarted";
      /**
       * The tool.
       */
      toolId: string;
    }
  | {
      error: ErrorEnvelope;
      /**
       * The invocation.
       */
      invocationId: string;
      kind: "toolFailed";
    }
  | {
      /**
       * Short, safe description of the result.
       */
      displaySummary: string;
      /**
       * The invocation this closes.
       */
      invocationId: string;
      kind: "toolCompleted";
      /**
       * What the invocation actually changed.
       */
      sideEffect: "none" | "localReversible" | "localIrreversible" | "external";
      /**
       * Whether output was cut short by a limit.
       */
      truncated: boolean;
    }
  | {
      /**
       * The checkpoint.
       */
      checkpointId: string;
      /**
       * How many steps it records as already done.
       */
      completedStepCount: number;
      /**
       * The durable sequence it was taken at.
       */
      eventSequence: number;
      kind: "checkpointSaved";
      /**
       * What was in flight, when anything was.
       */
      pendingAction?: PendingAction | null;
      resume: ResumeEligibility;
    }
  | {
      kind: "taskCompleted";
      /**
       * Secret-free summary of the outcome.
       */
      summary: string;
    }
  | {
      error: ErrorEnvelope;
      kind: "taskFailed";
    }
  | {
      kind: "taskCancelled";
      /**
       * Who or what asked for cancellation.
       */
      origin: "user" | "system";
    }
  | {
      kind: "unsupported";
    };

/**
 * Where work ran.
 *
 * Recorded on routing decisions and shown to the user, so "did this leave my
 * machine?" is answerable from the durable record alone.
 */
export type ExecutionLocation =
  | {
      /**
       * Human-readable name for the interface.
       */
      displayName: string;
      kind: "local";
      /**
       * Stable identifier for the compute target.
       */
      targetId: string;
    }
  | {
      kind: "cloud";
      /**
       * Stable provider identifier.
       */
      providerId: string;
      /**
       * Provider region, when the provider exposes one.
       */
      region?: string;
    };

/**
 * How long an authorization lasts.
 */
export type GrantKind = "implicitSafeDefault" | "once" | "project";

/**
 * Whether a provider or worker is usable right now.
 */
export type Health = "unknown" | "available" | "degraded" | "unavailable";

/**
 * How a worker is integrated.
 */
export type IntegrationMode = "embedded" | "acp" | "customProcess";

/**
 * One tool invocation.
 */
export type InvocationId = string;

/**
 * A durable message in a conversation.
 */
export type MessageId = string;

/**
 * One piece of a message.
 */
export type MessagePart =
  | {
      kind: "text";
      /**
       * The text.
       */
      text: string;
    }
  | {
      /**
       * Arguments, already validated against the tool's schema.
       */
      arguments: {
        [k: string]: unknown;
      };
      /**
       * The invocation this call opened.
       */
      invocationId: string;
      kind: "toolCall";
      /**
       * Namespaced tool identifier.
       */
      toolId: string;
    }
  | {
      /**
       * Structured, redacted result content.
       */
      content: {
        [k: string]: unknown;
      };
      /**
       * The invocation this result closes.
       */
      invocationId: string;
      kind: "toolResult";
      /**
       * Whether output was cut short by a limit.
       */
      truncated: boolean;
    }
  | {
      kind: "unsupported";
    };

/**
 * Who produced a message.
 */
export type MessageRole = "system" | "user" | "assistant" | "tool";

/**
 * Whether a message is finished.
 */
export type MessageStatus = "committed" | "partial" | "superseded";

/**
 * Something a model can do.
 */
export type ModelCapability = "text" | "streaming" | "toolCalls" | "structuredOutput" | "imageInput" | "cancellation";

/**
 * Who decides which model a worker uses.
 */
export type ModelControl = "tramontanaBrokered" | "workerManaged";

/**
 * A model a provider offers.
 */
export interface ModelDescriptor {
  /**
   * Detected capabilities.
   */
  capabilities: ModelCapability[];
  /**
   * Context window, when the provider reports one.
   */
  contextLimit?: number;
  /**
   * Human-readable name.
   */
  displayName: string;
  /**
   * Current health.
   */
  health: "unknown" | "available" | "degraded" | "unavailable";
  /**
   * Stable reason for a non-available health, for diagnostics and tests.
   */
  healthReasonCode?: string;
  /**
   * Whether inference runs on this device or elsewhere.
   */
  location: "local" | "cloud";
  /**
   * Provider-scoped model identifier.
   */
  modelId: string;
  /**
   * Provider that offers it.
   */
  providerId: string;
}

/**
 * Whether a model runs on this device.
 */
export type ModelLocation = "local" | "cloud";

/**
 * What a worker can do on the network.
 */
export type NetworkControl = "denied" | "brokered" | "constrained" | "unobservable";

/**
 * A durable, provider-neutral message.
 */
export interface NormalizedMessage {
  /**
   * Conversation this message belongs to.
   */
  conversationId: string;
  /**
   * When it was created.
   */
  createdAt: string;
  /**
   * Unique identifier.
   */
  messageId: string;
  /**
   * Its content.
   */
  parts: MessagePart[];
  /**
   * Who produced it.
   */
  role: "system" | "user" | "assistant" | "tool";
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * Whether it is finished.
   */
  status: "committed" | "partial" | "superseded";
  /**
   * Task that produced it, when one did.
   */
  taskId?: TaskId | null;
}

/**
 * The one action that had started and not finished.
 */
export interface PendingAction {
  /**
   * Short, secret-free description for a person deciding what to do.
   */
  displaySummary: string;
  /**
   * What kind of action it was.
   */
  kind: "model" | "tool" | "approval" | "worker";
  /**
   * The invocation, request, or approval it refers to.
   */
  referenceId: string;
  /**
   * Whether repeating it is safe.
   */
  replaySafety: "safe" | "unsafe" | "unknown";
}

/**
 * What kind of work was in flight when the task stopped.
 */
export type PendingActionKind = "model" | "tool" | "approval" | "worker";

/**
 * Who asked for a capability.
 *
 * An audit record that cannot say *who* asked is not an audit record. A
 * built-in tool invoked by the user and the same tool invoked by an external
 * worker are different events, even when the arguments match.
 */
export type PermissionOrigin =
  | {
      kind: "user";
    }
  | {
      /**
       * Namespaced worker identifier.
       */
      agentId: string;
      kind: "worker";
    }
  | {
      kind: "workflow";
      /**
       * Workflow identifier.
       */
      workflowId: string;
    };

/**
 * A project: one or more user-approved roots and the work done against them.
 */
export type ProjectId = string;

/**
 * A project and the roots the user approved for it.
 */
export interface ProjectRecord {
  /**
   * When the project was created.
   */
  createdAt: string;
  /**
   * Display name.
   */
  name: string;
  /**
   * Stable identifier.
   */
  projectId: string;
  /**
   * Canonical approved roots. Device-local bindings, redacted on export.
   */
  roots: string[];
  /**
   * Contract schema version.
   */
  schemaVersion: number;
}

/**
 * Whether repeating an action is safe.
 *
 * This is a statement about *effects*, not about whether the action would
 * succeed. Reading a file twice is safe; writing one twice may not be, and a
 * tool whose effects are not known is treated as though they are not.
 */
export type ReplaySafety = "safe" | "unsafe" | "unknown";

/**
 * Whether, and how, this checkpoint can be resumed.
 */
export interface ResumeEligibility {
  /**
   * Whether resuming is offered at all.
   */
  eligible: boolean;
  /**
   * How, when it is.
   */
  mode?: ResumeMode | null;
  /**
   * Stable reason, present whenever the answer needs explaining.
   */
  reasonCode?: string;
}

/**
 * How a task may be picked up again.
 */
export type ResumeMode = "sameWorker" | "canonicalHandoff";

/**
 * How much damage a tool invocation could do if the model asked for it wrongly.
 */
export type RiskClass = "read" | "modify" | "dangerous";

/**
 * The user's standing policy for where inference may run.
 */
export type RoutingMode = "localOnly" | "automatic" | "cloudPreferred";

/**
 * An opaque handle to a credential.
 *
 * This is the only representation of a secret that may be persisted, logged,
 * exported, or sent over IPC. `key` is an application-generated identifier
 * and never carries any part of the value it refers to
 * (`SECURITY_AND_PERMISSIONS.md` §7).
 */
export interface SecretRef {
  /**
   * Opaque application-generated key. Never the secret.
   */
  key: string;
  /**
   * Which store resolves this reference.
   */
  store: "platform";
}

/**
 * Where a resolvable secret lives.
 *
 * Only `Platform` exists today. It is an enum rather than a bare marker so
 * that adding a store later is an additive schema change.
 */
export type SecretStore = "platform";

/**
 * What an invocation actually changed, recorded alongside its result.
 */
export type SideEffect = "none" | "localReversible" | "localIrreversible" | "external";

/**
 * One frame of an in-flight model stream.
 *
 * Explicitly **not** an [`EventEnvelope`]: frames are transport, are never
 * persisted, and carry no sequence in the durable stream. A reconnecting
 * interface rebuilds from durable events plus, optionally, the current
 * stream snapshot.
 */
export interface StreamFrame {
  /**
   * Monotonically increasing position within this stream.
   */
  offset: number;
  /**
   * Correlates with the `modelStarted` event that opened this stream.
   */
  streamId: string;
  /**
   * The text produced.
   */
  text: string;
}

/**
 * A point a task can be understood from and continued at.
 *
 * Two fields deviate from the shape written in `CORE_CONTRACTS.md` §11, and
 * the document has been updated to match: `worker`, `contextManifestId`, and
 * `configurationManifestHash` are optional here. A task interrupted while
 * `preparing` has not selected a worker or prepared a context, and a
 * projectless task never has a configuration manifest
 * (`CORE_CONTRACTS.md` §5.2). Requiring them would mean either inventing
 * values or refusing to checkpoint exactly the tasks a checkpoint is most
 * needed for.
 */
export interface TaskCheckpoint {
  /**
   * Stable identifier.
   */
  checkpointId: string;
  /**
   * Steps already done, which resume must not repeat.
   *
   * The watermark, not a log: a step named here has a durable completion
   * event behind it.
   */
  completedStepIds: string[];
  /**
   * The effective configuration in force, for a project task.
   */
  configurationManifestHash?: string;
  /**
   * The context bundle prepared for this task, when one was.
   */
  contextManifestId?: string;
  /**
   * When it was written.
   */
  createdAt: string;
  /**
   * The durable sequence this checkpoint was taken at.
   *
   * Everything at or below this sequence is accounted for by the fields
   * here; anything above it happened afterwards.
   */
  eventSequence: number;
  /**
   * The action that had started and not finished, when there was one.
   */
  pendingAction?: PendingAction | null;
  resume: ResumeEligibility;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * The task this describes.
   */
  taskId: string;
  /**
   * The task's state at that sequence.
   */
  taskState:
    | "queued"
    | "preparing"
    | "routing"
    | "startingWorker"
    | "running"
    | "awaitingApproval"
    | "checkpointing"
    | "cancelling"
    | "interrupted"
    | "completed"
    | "failed"
    | "cancelled";
  /**
   * The worker, when one had been selected.
   */
  worker?: CheckpointWorker | null;
}

/**
 * What a durable event is about.
 *
 * `#[non_exhaustive]` and the `Unsupported` fallback in
 * [`EventPayload`] together implement the compatibility rule in
 * `CORE_CONTRACTS.md` §2: an unknown event kind is stored and shown as an
 * unsupported redacted event, never a crash.
 */
export type TaskEventKind =
  | "taskCreated"
  | "taskStateChanged"
  | "configurationResolved"
  | "contextPrepared"
  | "routingDecided"
  | "workerSelected"
  | "workerStarted"
  | "modelStarted"
  | "messageCommitted"
  | "toolRequested"
  | "approvalRequested"
  | "approvalResolved"
  | "toolStarted"
  | "toolCompleted"
  | "toolFailed"
  | "checkpointSaved"
  | "artifactRecorded"
  | "workerStopped"
  | "taskCompleted"
  | "taskFailed"
  | "taskCancelled"
  | "diagnosticRecorded";

/**
 * A unit of work with its own event stream and exactly one terminal state.
 */
export type TaskId = string;

/**
 * A task row.
 *
 * `state` and `last_sequence` are a cache of the event stream, not a second
 * source of truth. After an unclean stop they are rebuilt from the events.
 */
export interface TaskRecord {
  /**
   * Effective-configuration manifest captured before this task started.
   */
  configurationManifestHash?: string;
  /**
   * Conversation this belongs to, when it has one.
   */
  conversationId?: ConversationId | null;
  /**
   * When the task was created.
   */
  createdAt: string;
  /**
   * Secret-free summary of what was asked for.
   */
  goalSummary: string;
  /**
   * Highest durable sequence written.
   */
  lastSequence: number;
  /**
   * Project this belongs to, when it has one.
   */
  projectId?: ProjectId | null;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * Cached state, projected from the events.
   */
  state:
    | "queued"
    | "preparing"
    | "routing"
    | "startingWorker"
    | "running"
    | "awaitingApproval"
    | "checkpointing"
    | "cancelling"
    | "interrupted"
    | "completed"
    | "failed"
    | "cancelled";
  /**
   * Stable identifier.
   */
  taskId: string;
  /**
   * When the projection was last updated.
   */
  updatedAt: string;
}

/**
 * The current view of a task.
 *
 * A projection of the event stream, not a second source of truth: it exists
 * so reading a task's state does not require replaying every event.
 */
export interface TaskSnapshot {
  /**
   * Highest durable sequence written.
   */
  lastSequence: number;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * Current state.
   */
  state:
    | "queued"
    | "preparing"
    | "routing"
    | "startingWorker"
    | "running"
    | "awaitingApproval"
    | "checkpointing"
    | "cancelling"
    | "interrupted"
    | "completed"
    | "failed"
    | "cancelled";
  /**
   * Stable identifier.
   */
  taskId: string;
}

/**
 * Where a task is in its lifecycle.
 */
export type TaskState =
  | "queued"
  | "preparing"
  | "routing"
  | "startingWorker"
  | "running"
  | "awaitingApproval"
  | "checkpointing"
  | "cancelling"
  | "interrupted"
  | "completed"
  | "failed"
  | "cancelled";

/**
 * One item delivered to a task subscriber.
 *
 * Durable events and ephemeral frames share a channel so a subscriber sees
 * them interleaved in the order they happened, but they stay distinct
 * variants: only [`TaskUpdate::Event`] carries a sequence, and only it
 * survives a restart. An interface that treats a frame as durable will lose
 * it on reconnect and be surprised; the type makes that hard to do by
 * accident.
 */
export type TaskUpdate =
  | {
      event: EventEnvelope;
      kind: "event";
    }
  | {
      frame: StreamFrame;
      kind: "frame";
    };

/**
 * An instant, serialized as a UTC RFC 3339 string.
 */
export type Timestamp = string;

/**
 * How a worker reaches tools.
 */
export type ToolControl = "tramontanaBrokered" | "directConstrained" | "directUnconstrained";

/**
 * A tool the registry can offer to a worker.
 */
export interface ToolDescriptor {
  /**
   * Capability it requires.
   */
  capability: string;
  /**
   * What it does, for the approval prompt. Presentation, never policy.
   */
  description: string;
  /**
   * Human-readable name.
   */
  displayName: string;
  /**
   * JSON Schema its arguments are validated against.
   */
  inputSchema: {
    [k: string]: unknown;
  };
  /**
   * Provider that implements it.
   */
  providerId: string;
  /**
   * How much damage it could do.
   */
  riskClass: "read" | "modify" | "dangerous";
  /**
   * What it changes.
   */
  sideEffect: "none" | "localReversible" | "localIrreversible" | "external";
  /**
   * Whether it honors cancellation.
   */
  supportsCancellation: boolean;
  /**
   * Namespaced identifier, such as `builtin.fs.read` or `mcp.github.search`.
   */
  toolId: string;
}

/**
 * Who an event is for.
 *
 * Redaction happens in core before delivery, so `Secret` events are internal
 * audit markers holding references and hashes only, and never reach a webview
 * (`CORE_CONTRACTS.md` §5).
 */
export type Visibility = "user" | "diagnostic" | "secret";
