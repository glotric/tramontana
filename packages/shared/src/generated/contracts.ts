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
 * One file in a bundle, with what a reader needs to trust it.
 */
export interface BundleEntry {
  /**
   * Size in bytes, checked against the contents before the checksum.
   */
  byteCount: number;
  /**
   * Bundle-relative path. Validated on import before it is used.
   */
  path: string;
  /**
   * Lowercase hex SHA-256 of the contents.
   */
  sha256: string;
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
  projectId?: string;
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
 * Where a piece of context came from.
 */
export type ContextCategory =
  "conversation" | "selectedFile" | "instruction" | "skill" | "repositoryMetadata" | "toolResult";

/**
 * Everything assembled for one destination.
 */
export interface ContextManifest {
  /**
   * Stable identifier, referenced by routing decisions and checkpoints.
   */
  contextManifestId: string;
  /**
   * When it was assembled.
   */
  createdAt: string;
  /**
   * Where it was going.
   */
  destinationClass: "local" | "cloud" | "worker";
  /**
   * Stable codes for redactions applied before release.
   */
  redactions: string[];
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * Everything considered, included or not.
   */
  sources: ContextSource[];
  /**
   * The task this was assembled for.
   */
  taskId: string;
  /**
   * Total bytes actually sent.
   */
  totalBytes: number;
  /**
   * Approximate total token cost, when estimated.
   */
  totalTokenEstimate?: number;
}

/**
 * One thing that was considered for a context bundle.
 */
export interface ContextSource {
  /**
   * Size of the content in bytes.
   */
  byteCount: number;
  /**
   * What kind of source it is.
   */
  category: "conversation" | "selectedFile" | "instruction" | "skill" | "repositoryMetadata" | "toolResult";
  /**
   * Hash of the content, so the manifest explains without copying.
   */
  contentHash: string;
  /**
   * How sensitive its content is.
   */
  dataClass: "public" | "project" | "sensitive" | "secret";
  /**
   * Why it was not, when it was not.
   */
  exclusionReason?: string;
  /**
   * Whether it was actually sent.
   */
  included: boolean;
  /**
   * Stable identifier for this source within the task.
   */
  sourceId: string;
  /**
   * Approximate token cost, when one was estimated.
   */
  tokenEstimate?: number;
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
  archivedAt?: string;
  /**
   * Stable identifier.
   */
  conversationId: string;
  /**
   * When it was created.
   */
  createdAt: string;
  /**
   * Project this belongs to. Absent means projectless, which carries no
   * implicit filesystem access (`PHASE_1_ACCEPTANCE_CRITERIA.md` §8).
   */
  projectId?: string;
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
 * Where assembled context is about to go.
 *
 * The distinction is a policy one, not a technical one: crossing to `Cloud`
 * is a data-release decision that needs a disclosure, and `Worker` means a
 * process that is not the model and not Tramontana
 * (`SECURITY_AND_PERMISSIONS.md` §8).
 */
export type DestinationClass = "local" | "cloud" | "worker";

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
  conversationId?: string;
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
         * Stable machine-readable code.
         */
        code: string;
        kind: "diagnosticRecorded";
        /**
         * Secret-free description.
         */
        summary: string;
      }
    | {
        /**
         * Conversation this task belongs to, when it has one.
         */
        conversationId?: string;
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
        kind: "contextPrepared";
        manifest: ContextManifest;
      }
    | {
        decision: RoutingDecision;
        kind: "routingDecided";
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
         * Hash of the tool identity and normalized arguments, never raw values.
         */
        requestHash?: string;
        /**
         * How much damage the tool could do.
         */
        riskClass: "read" | "modify" | "dangerous";
        /**
         * Worker-supplied stable step identity, scoped to this task.
         */
        stepId?: string;
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
        grantKind?: "implicitSafeDefault" | "once" | "project";
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
         * Effect classification recorded before execution. Legacy events are unknown.
         */
        replaySafety: "safe" | "unsafe" | "unknown";
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
        pendingAction?: PendingAction;
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
   * Task this event belongs to.
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
       * Stable machine-readable code.
       */
      code: string;
      kind: "diagnosticRecorded";
      /**
       * Secret-free description.
       */
      summary: string;
    }
  | {
      /**
       * Conversation this task belongs to, when it has one.
       */
      conversationId?: string;
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
      kind: "contextPrepared";
      manifest: ContextManifest;
    }
  | {
      decision: RoutingDecision;
      kind: "routingDecided";
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
       * Hash of the tool identity and normalized arguments, never raw values.
       */
      requestHash?: string;
      /**
       * How much damage the tool could do.
       */
      riskClass: "read" | "modify" | "dangerous";
      /**
       * Worker-supplied stable step identity, scoped to this task.
       */
      stepId?: string;
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
      grantKind?: "implicitSafeDefault" | "once" | "project";
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
       * Effect classification recorded before execution. Legacy events are unknown.
       */
      replaySafety: "safe" | "unsafe" | "unknown";
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
      pendingAction?: PendingAction;
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
 * The bundle's table of contents.
 */
export interface ExportManifest {
  /**
   * Major version of the bundle layout. Refused when unrecognized.
   */
  bundleMajorVersion: number;
  /**
   * Minor version. A reader may ignore what it does not know.
   */
  bundleMinorVersion: number;
  /**
   * When the bundle was produced.
   */
  createdAt: string;
  /**
   * Every file in the bundle.
   */
  entries: BundleEntry[];
  /**
   * What was left out, and how much.
   */
  omissions: Omission[];
  options: ExportOptions;
  /**
   * The project, when the export was scoped to one.
   */
  projectId?: string;
  /**
   * Version of the records inside the bundle.
   */
  schemaVersion: number;
  /**
   * Tasks included, in export order.
   */
  taskIds: TaskId[];
}

/**
 * What an export chose to include.
 *
 * Absent means the default, which is to exclude. The flags exist so a bundle
 * records the decision that produced it rather than leaving a reader to infer
 * it from what happens to be present.
 */
export interface ExportOptions {
  /**
   * Keep absolute device paths instead of redacting them.
   */
  includeAbsolutePaths: boolean;
  /**
   * Include artifact files.
   */
  includeArtifacts: boolean;
  /**
   * Keep unredacted tool output.
   */
  includeRawToolOutput: boolean;
}

/**
 * A configuration manifest with the project it belongs to.
 *
 * The manifest itself does not name its project — it is stored under one —
 * so a bundle has to carry the pairing. Without it an imported task would
 * reference a manifest nothing could resolve, and the record could not say
 * which instructions were in force when the work happened.
 */
export interface ExportedConfiguration {
  configuration: EffectiveConfiguration;
  /**
   * The project this manifest was resolved for.
   */
  projectId: string;
}

/**
 * How long an authorization lasts.
 */
export type GrantKind = "implicitSafeDefault" | "once" | "project";

/**
 * Everything a different worker needs to continue a task.
 *
 * Assembled from canonical records only. A worker's opaque continuation data
 * is never read to build this, which is what makes the claim in
 * `PROJECT_FORMAT.md` §4 — that canonical state is sufficient — testable
 * rather than aspirational.
 */
export interface HandoffBundle {
  /**
   * Steps already done, which the receiving worker must not repeat.
   */
  completedStepIds: string[];
  /**
   * The effective configuration in force, for a project task.
   */
  configurationManifestHash?: string;
  /**
   * Its conversation, when it has one.
   */
  conversationId?: string;
  /**
   * When the handoff was assembled.
   */
  createdAt: string;
  /**
   * The point being handed over.
   */
  eventSequence: number;
  /**
   * What the user asked for.
   */
  goal: string;
  /**
   * The task's normalized history.
   */
  history: HandoffEntry[];
  /**
   * What this handoff does not carry.
   */
  omissions: Omission[];
  pendingAction?: PendingAction;
  previousWorker?: AgentDescriptor;
  /**
   * Its project, when it has one.
   */
  projectId?: string;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * The task being handed over.
   */
  taskId: string;
}

/**
 * One fact in a handoff, normalized so any worker can read it.
 */
export interface HandoffEntry {
  /**
   * What kind of fact this is.
   */
  kind: string;
  /**
   * When it happened.
   */
  occurredAt: string;
  /**
   * Position in the task's durable stream.
   */
  sequence: number;
  /**
   * A secret-free description.
   */
  summary: string;
}

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
 * One item from a model stream.
 *
 * Exactly one [`ModelEvent::Completed`] or [`ModelEvent::Failed`] ends a
 * stream, even when cancellation arrives while the underlying request is
 * still in flight (`CORE_CONTRACTS.md` §8). The shared provider contract
 * suite checks that for every adapter.
 */
export type ModelEvent =
  | {
      kind: "textDelta";
      /**
       * Position within this stream.
       */
      offset: number;
      /**
       * The stream this belongs to.
       */
      streamId: string;
      /**
       * The text produced.
       */
      text: string;
    }
  | {
      /**
       * Identifies the call being assembled across deltas.
       */
      callId: string;
      /**
       * The fragment.
       */
      delta: string;
      kind: "toolCallDelta";
      /**
       * Position within this stream.
       */
      offset: number;
      /**
       * The stream this belongs to.
       */
      streamId: string;
    }
  | {
      kind: "usage";
      usage: Usage;
    }
  | {
      kind: "completed";
      /**
       * Why it ended.
       */
      stopReason: "stop" | "length" | "toolCall" | "cancelled";
    }
  | {
      error: ErrorEnvelope;
      kind: "failed";
    };

/**
 * Bounds on one request.
 *
 * `deadlineMs` is required rather than optional: a request with no deadline
 * is one that can hang a task forever, and every provider has to honour the
 * same bound for cancellation to mean the same thing everywhere.
 */
export interface ModelLimits {
  /**
   * How long the whole request may take.
   */
  deadlineMs: number;
  /**
   * Cap on generated tokens, when one applies.
   */
  maxOutputTokens?: number;
}

/**
 * Whether a model runs on this device.
 */
export type ModelLocation = "local" | "cloud";

/**
 * A normalized request for inference.
 */
export interface ModelRequest {
  /**
   * The context release this request was built from.
   */
  contextManifestId: string;
  /**
   * How sensitive the content is, for routing and disclosure.
   */
  dataClasses?: DataClass[];
  limits: ModelLimits;
  /**
   * The conversation so far, already normalized and redacted.
   */
  messages: NormalizedMessage[];
  /**
   * Correlates this request with its `modelStarted` event and its stream.
   */
  requestId: string;
  /**
   * Capabilities a model must have to serve this request.
   *
   * The router matches these; a provider that cannot meet them is not a
   * fallback, it is ineligible.
   */
  requiredCapabilities?: string[];
  /**
   * What shape of answer was asked for.
   */
  responseFormat?:
    | {
        kind: "text";
      }
    | {
        kind: "jsonSchema";
        /**
         * The schema the answer must satisfy.
         */
        schema: {
          [k: string]: unknown;
        };
      };
  /**
   * Contract schema version.
   */
  schemaVersion: number;
  /**
   * The task on whose behalf it is made.
   */
  taskId: string;
  /**
   * Tools the model may call, as the model sees them.
   */
  tools?: NormalizedToolDefinition[];
}

/**
 * What a worker can do on the network.
 */
export type NetworkControl = "denied" | "brokered" | "constrained" | "unobservable";

/**
 * A durable, provider-neutral message.
 */
export interface NormalizedMessage {
  /**
   * Conversation this message belongs to, when it has one.
   *
   * Optional because a task can be started outside a conversation, and its
   * assistant answer is still the durable record of what the model said.
   * Dropping the message because there was no conversation would leave the
   * exported task with tool calls and no answer.
   */
  conversationId?: string;
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
  taskId?: string;
}

/**
 * A tool as a model is told about it.
 *
 * Deliberately not [`ToolDescriptor`](super::ToolDescriptor): that type
 * carries the risk class, capability, and cancellation facts Tramontana uses
 * to *govern* a call. A model needs the name, the purpose, and the argument
 * shape, and nothing else.
 */
export interface NormalizedToolDefinition {
  /**
   * What it does, in the words the model sees.
   */
  description: string;
  /**
   * JSON Schema for the arguments.
   */
  inputSchema: {
    [k: string]: unknown;
  };
  /**
   * Namespaced tool identifier, as the model should call it.
   */
  name: string;
}

/**
 * One thing an export left out, and how much of it.
 */
export interface Omission {
  /**
   * What kind of data.
   */
  category: "secret" | "absolutePath" | "rawToolOutput" | "artifact" | "internalEvent" | "workerPrivateState";
  /**
   * How many values were affected.
   */
  count: number;
  /**
   * Why it was left out, in a sentence a person can act on.
   */
  reason: string;
}

/**
 * What kind of data was left out of an export.
 */
export type OmissionCategory =
  "secret" | "absolutePath" | "rawToolOutput" | "artifact" | "internalEvent" | "workerPrivateState";

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
 * What shape of answer was asked for.
 */
export type ResponseFormat =
  | {
      kind: "text";
    }
  | {
      kind: "jsonSchema";
      /**
       * The schema the answer must satisfy.
       */
      schema: {
        [k: string]: unknown;
      };
    };

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
  mode?: "sameWorker" | "canonicalHandoff";
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
 * One model a decision could use.
 */
export interface RoutedModel {
  /**
   * The model.
   */
  modelId: string;
  /**
   * The provider offering it.
   */
  providerId: string;
}

/**
 * Where a task's inference was sent, and why.
 */
export interface RoutingDecision {
  /**
   * The availability snapshot the decision was made against.
   *
   * Recorded so a decision can be explained later even though availability
   * has moved on. Without it, "why not the other model?" is unanswerable.
   */
  availabilitySnapshotId: string;
  /**
   * The context this decision released.
   */
  contextManifestId: string;
  /**
   * Stable identifier for this decision.
   */
  decisionId: string;
  /**
   * What would have been tried next, in order.
   */
  fallbackOrder: RoutingFallback[];
  /**
   * Where it runs.
   */
  location:
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
   * The routing mode in force when it was made.
   */
  mode: "localOnly" | "automatic" | "cloudPreferred";
  model: RoutedModel;
  /**
   * Why, in deterministic order.
   */
  reasonCodes: string[];
  /**
   * Whether reaching this route needs the user told.
   */
  requiresDisclosure: boolean;
  /**
   * Contract schema version.
   */
  schemaVersion: number;
}

/**
 * A model and where it would run, as a fallback option.
 */
export interface RoutingFallback {
  /**
   * Where it would run.
   */
  location:
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
   * The model.
   */
  modelId: string;
  /**
   * The provider offering it.
   */
  providerId: string;
}

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
 * Why a model stream ended.
 */
export type StopReason = "stop" | "length" | "toolCall" | "cancelled";

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
  pendingAction?: PendingAction;
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
  worker?: CheckpointWorker;
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
  conversationId?: string;
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
  projectId?: string;
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
 * Token counts for one request.
 */
export interface Usage {
  /**
   * Whether these are counted or approximated.
   *
   * Recorded rather than hidden: a cost shown to a user as exact when it was
   * estimated is a number they will make decisions on.
   */
  estimated: boolean;
  /**
   * Tokens consumed by the request.
   */
  inputTokens?: number;
  /**
   * Tokens produced.
   */
  outputTokens?: number;
}

/**
 * Who an event is for.
 *
 * Redaction happens in core before delivery, so `Secret` events are internal
 * audit markers holding references and hashes only, and never reach a webview
 * (`CORE_CONTRACTS.md` §5).
 */
export type Visibility = "user" | "diagnostic" | "secret";
