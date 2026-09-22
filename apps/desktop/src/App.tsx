import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isSchemaCompatible, SCHEMA_VERSION } from "@tramontana/shared";
import type {
  ApprovalRequest,
  ConversationRecord,
  EventEnvelope,
  ProjectRecord,
  RoutingMode,
  TaskRecord,
  TaskUpdate,
} from "@tramontana/shared";

import { describeEvent, isTerminal, projectState } from "./activity.js";
import { ActivityCards } from "./ActivityCards.js";
import { ApprovalCard } from "./ApprovalCard.js";
import { BundlesPanel } from "./BundlesPanel.js";
import { buildCards, describeHeader, MODE_LABEL } from "./cards.js";
import { ChangesPanel } from "./ChangesPanel.js";
import { GrantsPanel } from "./GrantsPanel.js";
import { AppsView } from "./AppsView.js";
import { ModelsView } from "./ModelsView.js";
import { foldFrame } from "./stream.js";
import type { LiveStream } from "./stream.js";
import { TaskHeader } from "./TaskHeader.js";
import { CheckpointPanel } from "./CheckpointPanel.js";
import {
  asErrorEnvelope,
  cancelTask,
  createConversation,
  fetchCoreInfo,
  inspectProject,
  listConversations,
  listPendingApprovals,
  listProjects,
  listTasks,
  onApprovalRequested,
  openProject,
  replayTaskEvents,
  startTask,
  watchTask,
} from "./core.js";
import type { ProjectInspection } from "./core.js";

type CoreStatus =
  | { readonly kind: "connecting" }
  | { readonly kind: "ready"; readonly appVersion: string }
  | { readonly kind: "incompatible"; readonly reported: number }
  | { readonly kind: "unavailable" };

function describeCoreStatus(status: CoreStatus): string {
  switch (status.kind) {
    case "connecting":
      return "Connecting to core…";
    case "ready":
      return `Core ${status.appVersion} · contracts v${SCHEMA_VERSION}`;
    case "incompatible":
      return `Core speaks contracts v${status.reported}; this build speaks v${SCHEMA_VERSION}`;
    case "unavailable":
      return "Core unavailable — open the desktop shell to use durable projects";
  }
}

function failureText(error: unknown, fallback: string): string {
  const envelope = asErrorEnvelope(error);
  return envelope ? `${envelope.code}: ${envelope.message}` : fallback;
}

function shortId(value: string): string {
  const parts = value.split("-");
  return parts.length > 2 ? `${parts[0]}-${parts.at(-1)?.slice(0, 8)}` : value;
}

/** Where a task's models may run, in the order the composer offers them. */
const MODES: readonly RoutingMode[] = ["localOnly", "automatic", "cloudPreferred"];

/** What each mode means for the task about to start. */
const MODE_TEXT: Record<RoutingMode, string> = {
  localOnly: "Local only: models run on this device, and nothing is sent to a cloud model.",
  automatic:
    "Automatic: a model is chosen for you, and anything sent to a cloud model is asked about first.",
  cloudPreferred:
    "Prefer cloud: a cloud model is chosen when one is available, and each release to it is asked about first.",
};

/**
 * Say, before the task starts, what the person's choices let it do.
 *
 * Local Only governs where models run, not what a command does: an approved
 * recipe can still reach the network, so choosing both says so (ADR 0028's
 * boundary statement, and the recipe prompt, say the same).
 */
function describeChoices(
  mode: RoutingMode,
  writes: boolean,
  recipes: boolean,
  apps: boolean,
  browse: boolean,
): string {
  const asks = [
    ...(writes ? ["edit files"] : []),
    ...(recipes ? ["run the project's recipes"] : []),
    ...(apps ? ["use the Apps' reading tools"] : []),
    ...(browse ? ["open websites in a window of its own"] : []),
  ];
  const access =
    asks.length === 0
      ? "Read-only: the model can look at the project but change nothing."
      : `The model may ask to ${asks.join(" and ")}; each one is put to you before it runs.`;
  const commands =
    mode === "localOnly" && recipes
      ? " Local only governs where models run, not the commands a recipe runs."
      : "";
  const browsing = browse
    ? mode === "localOnly"
      ? " Each new site is asked about once, and every form. Local only governs where models run, not the pages the browser opens."
      : " Each new site is asked about once, and every form."
    : "";
  return `${MODE_TEXT[mode]} ${access}${commands}${browsing}`;
}

/**
 * Why a task's worker stopped, if the latest stop was the worker's rather
 * than a person's (ADR 0048).
 */
function stoppedBecause(events: readonly EventEnvelope[]): string | undefined {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const payload = events[index]?.payload;
    if (payload?.kind === "taskStateChanged" && payload.current !== "interrupted") return undefined;
    if (payload?.kind === "diagnosticRecorded" && payload.code.startsWith("worker.stopped.")) {
      return payload.summary;
    }
  }
  return undefined;
}

/** M2 desktop surface over the same durable service and profile as the CLI. */
export function App(): React.JSX.Element {
  const [status, setStatus] = useState<CoreStatus>({ kind: "connecting" });
  const [projects, setProjects] = useState<readonly ProjectRecord[]>([]);
  const [inspection, setInspection] = useState<ProjectInspection | undefined>();
  const [conversations, setConversations] = useState<readonly ConversationRecord[]>([]);
  const [selectedConversationId, setSelectedConversationId] = useState<string | undefined>();
  const [tasks, setTasks] = useState<readonly TaskRecord[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | undefined>();
  const [events, setEvents] = useState<readonly EventEnvelope[]>([]);
  const [stream, setStream] = useState<LiveStream | undefined>();
  const [goal, setGoal] = useState("Say hello");
  // Read-only and Automatic until the person says otherwise, for each task.
  const [routingMode, setRoutingMode] = useState<RoutingMode>("automatic");
  const [mayWrite, setMayWrite] = useState(false);
  const [mayRunRecipes, setMayRunRecipes] = useState(false);
  const [mayUseApps, setMayUseApps] = useState(false);
  const [mayBrowse, setMayBrowse] = useState(false);
  const [conversationTitle, setConversationTitle] = useState("");
  const [approvals, setApprovals] = useState<readonly ApprovalRequest[]>([]);
  // Moves on whenever a task ends, so the Changes panel reads the tree again.
  const [revision, setRevision] = useState(0);
  // The work area, or the models and credentials the tasks run against.
  const [view, setView] = useState<"work" | "models" | "apps">("work");
  const [failure, setFailure] = useState<string | undefined>();
  const [loadingProject, setLoadingProject] = useState(false);
  const [openingProject, setOpeningProject] = useState(false);

  const projectRequest = useRef(0);
  const conversationRequest = useRef(0);
  const taskRequest = useRef(0);
  const seen = useRef(new Set<number>());

  // Kept in a ref so the update handler stays the same function for the life
  // of the window; a new one per conversation would ripple through every
  // selection callback and reload the project list.
  const refreshTasksRef = useRef<() => void>(() => undefined);

  // One handler for every stream of task updates, whether the task was just
  // started here or is being followed again after a reload.
  const handleUpdate = useCallback((update: TaskUpdate): void => {
    switch (update.kind) {
      case "event": {
        const { event } = update;
        if (seen.current.has(event.sequence)) return;
        seen.current.add(event.sequence);
        setEvents((previous) => [...previous, event]);
        // The turn's whole answer is now in the record, and its card
        // carries the text. Keeping the frames on screen as well would show
        // the same words twice and leave them there while the next turn ran.
        if (event.payload.kind === "messageCommitted") setStream(undefined);
        if (event.payload.kind === "configurationResolved") {
          const resolved = event.payload.configuration;
          setInspection((current) =>
            current === undefined ? current : { ...current, configuration: resolved },
          );
        }
        if (
          event.payload.kind === "taskCompleted" ||
          event.payload.kind === "taskFailed" ||
          event.payload.kind === "taskCancelled"
        ) {
          refreshTasksRef.current();
          setRevision((current) => current + 1);
        }
        break;
      }
      case "frame":
        setStream((current) => foldFrame(current, update.frame));
        break;
    }
  }, []);

  const selectTask = useCallback(
    async (taskId: string): Promise<void> => {
      const request = ++taskRequest.current;
      setSelectedTaskId(taskId);
      setStream(undefined);
      setFailure(undefined);
      try {
        const replayed = await replayTaskEvents(taskId);
        if (request !== taskRequest.current) return;
        seen.current = new Set(replayed.map((event) => event.sequence));
        setEvents(replayed);
        // A task still under way goes on in the shell whether or not this
        // window was watching, as it was not across a reload. Follow it from
        // the last event the record holds, rather than show a record that
        // stopped when the window did.
        const state = projectState(replayed);
        const last = replayed.at(-1)?.sequence;
        if (
          state !== undefined &&
          !isTerminal(state) &&
          state !== "interrupted" &&
          last !== undefined
        ) {
          await watchTask(taskId, last, (update) => {
            if (request === taskRequest.current) handleUpdate(update);
          });
        }
      } catch (error: unknown) {
        if (request === taskRequest.current) {
          setFailure(failureText(error, "Could not replay the task history."));
        }
      }
    },
    [handleUpdate],
  );

  const selectConversation = useCallback(
    async (conversationId: string): Promise<void> => {
      const request = ++conversationRequest.current;
      taskRequest.current += 1;
      setSelectedConversationId(conversationId);
      setSelectedTaskId(undefined);
      setTasks([]);
      setEvents([]);
      setStream(undefined);
      setFailure(undefined);
      try {
        const records = await listTasks(conversationId);
        if (request !== conversationRequest.current) return;
        setTasks(records);
        const latest = records[0];
        if (latest !== undefined) await selectTask(latest.taskId);
      } catch (error: unknown) {
        if (request === conversationRequest.current) {
          setFailure(failureText(error, "Could not load conversation tasks."));
        }
      }
    },
    [selectTask],
  );

  const selectProject = useCallback(
    async (projectId: string): Promise<void> => {
      const request = ++projectRequest.current;
      conversationRequest.current += 1;
      taskRequest.current += 1;
      setLoadingProject(true);
      setInspection(undefined);
      setConversations([]);
      setSelectedConversationId(undefined);
      setTasks([]);
      setSelectedTaskId(undefined);
      setEvents([]);
      setStream(undefined);
      setFailure(undefined);
      try {
        const [detail, records] = await Promise.all([
          inspectProject(projectId),
          listConversations(projectId),
        ]);
        if (request !== projectRequest.current) return;
        setInspection(detail);
        setConversations(records);
        const latest = records[0];
        if (latest !== undefined) await selectConversation(latest.conversationId);
      } catch (error: unknown) {
        if (request === projectRequest.current) {
          setFailure(failureText(error, "Could not load the selected project."));
        }
      } finally {
        if (request === projectRequest.current) setLoadingProject(false);
      }
    },
    [selectConversation],
  );

  useEffect(() => {
    let active = true;
    void fetchCoreInfo().then(
      (info) => {
        if (!active) return;
        setStatus(
          isSchemaCompatible(info.schemaVersion)
            ? { kind: "ready", appVersion: info.appVersion }
            : { kind: "incompatible", reported: info.schemaVersion },
        );
      },
      () => {
        if (active) setStatus({ kind: "unavailable" });
      },
    );
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;
    void listProjects().then(
      (records) => {
        if (!active) return;
        setProjects(records);
        const latest = records[0];
        if (latest !== undefined) void selectProject(latest.projectId);
      },
      (error: unknown) => {
        if (active) setFailure(failureText(error, "Could not load recent projects."));
      },
    );
    return () => {
      active = false;
      projectRequest.current += 1;
      conversationRequest.current += 1;
      taskRequest.current += 1;
    };
  }, [selectProject]);

  // One queue of open questions, in the order they were asked. Questions
  // raised before this window existed are asked for on start, because the
  // event that raised them is not replayed and their tasks are still waiting.
  // A shell that cannot be reached at all is already reported by the core
  // status line, so a failure here is not raised a second time.
  useEffect(() => {
    let active = true;
    const add = (request: ApprovalRequest): void => {
      setApprovals((current) =>
        current.some((waiting) => waiting.approvalId === request.approvalId)
          ? current
          : [...current, request],
      );
    };
    void listPendingApprovals().then((waiting) => {
      if (active) for (const request of waiting) add(request);
    }, undefined);
    const subscription = onApprovalRequested(add);
    return () => {
      active = false;
      void subscription.then((unsubscribe) => {
        unsubscribe();
      }, undefined);
    };
  }, []);

  const onApprovalAnswered = useCallback((approvalId: string) => {
    setApprovals((current) => current.filter((waiting) => waiting.approvalId !== approvalId));
    // An answer can grant a standing permission, which the inspector lists.
    setRevision((current) => current + 1);
  }, []);

  const selectedConversation = useMemo(
    () =>
      conversations.find((conversation) => conversation.conversationId === selectedConversationId),
    [conversations, selectedConversationId],
  );
  const selectedTask = useMemo(
    () => tasks.find((task) => task.taskId === selectedTaskId),
    [selectedTaskId, tasks],
  );
  const taskState = useMemo(() => projectState(events), [events]);
  const cards = useMemo(() => buildCards(events), [events]);
  const header = useMemo(() => describeHeader(events), [events]);
  const goalInput = useRef<HTMLTextAreaElement>(null);

  // Retry puts the goal back where the person chooses again; it starts nothing.
  const onRetry = useCallback((retried: string) => {
    setGoal(retried);
    goalInput.current?.focus();
  }, []);
  const running = selectedTaskId !== undefined && taskState !== undefined && !isTerminal(taskState);
  const configuration = inspection?.configuration ?? undefined;

  const onOpenProject = useCallback(() => {
    setOpeningProject(true);
    setFailure(undefined);
    void openProject()
      .then(async (opened) => {
        if (opened === undefined) return;
        const records = await listProjects();
        setProjects(records);
        await selectProject(opened.project.projectId);
      })
      .catch((error: unknown) => {
        setFailure(failureText(error, "Could not open the selected project."));
      })
      .finally(() => {
        setOpeningProject(false);
      });
  }, [selectProject]);

  const onCreateConversation = useCallback(() => {
    const projectId = inspection?.project.projectId;
    const title = conversationTitle.trim();
    if (projectId === undefined || title === "") return;
    setFailure(undefined);
    void createConversation(projectId, title).then(
      (created) => {
        setConversations((current) => [created, ...current]);
        setConversationTitle("");
        void selectConversation(created.conversationId);
      },
      (error: unknown) => {
        setFailure(failureText(error, "Could not create the conversation."));
      },
    );
  }, [conversationTitle, inspection?.project.projectId, selectConversation]);

  const refreshTasks = useCallback(() => {
    if (selectedConversationId === undefined) return;
    void listTasks(selectedConversationId).then(setTasks, () => undefined);
  }, [selectedConversationId]);
  useEffect(() => {
    refreshTasksRef.current = refreshTasks;
  }, [refreshTasks]);

  const onStart = useCallback(() => {
    if (selectedConversationId === undefined) {
      setFailure("Create or select a conversation before starting a task.");
      return;
    }
    // A new task on screen: updates from any stream opened for the one before
    // it are no longer this window's to show.
    const request = ++taskRequest.current;
    seen.current = new Set();
    setEvents([]);
    setStream(undefined);
    setFailure(undefined);
    setSelectedTaskId(undefined);

    const onUpdate = (update: TaskUpdate): void => {
      if (request === taskRequest.current) handleUpdate(update);
    };

    void startTask(goal, onUpdate, selectedConversationId, {
      writes: mayWrite,
      recipes: mayRunRecipes,
      apps: mayUseApps,
      browse: mayBrowse,
      routingMode,
    }).then(
      (id) => {
        setSelectedTaskId(id);
        refreshTasks();
      },
      (error: unknown) => {
        setFailure(failureText(error, "Could not start the task."));
      },
    );
  }, [
    goal,
    handleUpdate,
    mayBrowse,
    mayRunRecipes,
    mayUseApps,
    mayWrite,
    refreshTasks,
    routingMode,
    selectedConversationId,
  ]);

  const onCancel = useCallback(() => {
    if (selectedTaskId === undefined) return;
    void cancelTask(selectedTaskId).catch((error: unknown) => {
      setFailure(failureText(error, "Could not cancel the task."));
    });
  }, [selectedTaskId]);

  return (
    <div className="app-shell">
      <aside className="rail" aria-label="Projects and conversations">
        <div className="brand-lockup">
          <span className="brand-mark" aria-hidden="true">
            T
          </span>
          <div>
            <strong>Tramontana</strong>
            <span>Personal control plane</span>
          </div>
        </div>

        <button
          type="button"
          className="button button--primary button--block"
          onClick={onOpenProject}
          disabled={status.kind !== "ready" || openingProject}
        >
          {openingProject ? "Opening…" : "+ Open project"}
        </button>

        <nav className="rail-scroll" aria-label="Recent work">
          <section>
            <h2 className="rail-heading">Projects</h2>
            {projects.length === 0 ? (
              <p className="rail-empty">No durable projects yet.</p>
            ) : (
              <ul className="rail-list">
                {projects.map((project) => (
                  <li key={project.projectId}>
                    <button
                      type="button"
                      className="rail-row"
                      data-selected={inspection?.project.projectId === project.projectId}
                      aria-current={
                        inspection?.project.projectId === project.projectId ? "true" : undefined
                      }
                      onClick={() => void selectProject(project.projectId)}
                    >
                      <span className="status-dot" aria-hidden="true" />
                      <span>{project.name}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>

          {inspection !== undefined && (
            <section>
              <h2 className="rail-heading">Conversations</h2>
              <form
                className="new-conversation"
                onSubmit={(submitted) => {
                  submitted.preventDefault();
                  onCreateConversation();
                }}
              >
                <label className="sr-only" htmlFor="conversation-title">
                  New conversation title
                </label>
                <input
                  id="conversation-title"
                  value={conversationTitle}
                  onChange={(changed) => {
                    setConversationTitle(changed.target.value);
                  }}
                  placeholder="New conversation"
                />
                <button type="submit" disabled={conversationTitle.trim() === ""}>
                  Add
                </button>
              </form>
              {conversations.length === 0 ? (
                <p className="rail-empty">Create a conversation for this project.</p>
              ) : (
                <ul className="rail-list">
                  {conversations.map((conversation) => (
                    <li key={conversation.conversationId}>
                      <button
                        type="button"
                        className="rail-row rail-row--conversation"
                        data-selected={selectedConversationId === conversation.conversationId}
                        aria-current={
                          selectedConversationId === conversation.conversationId
                            ? "true"
                            : undefined
                        }
                        onClick={() => void selectConversation(conversation.conversationId)}
                      >
                        <span>{conversation.title}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          )}
        </nav>

        <button
          type="button"
          className="rail-row rail-row--settings"
          aria-pressed={view === "models"}
          onClick={() => {
            setView((current) => (current === "models" ? "work" : "models"));
          }}
        >
          Models and credentials
        </button>
        <button
          type="button"
          className="rail-row rail-row--settings"
          aria-pressed={view === "apps"}
          onClick={() => {
            setView((current) => (current === "apps" ? "work" : "apps"));
          }}
        >
          Apps
        </button>

        <footer className="rail-status">
          <span className="status-dot" data-state={status.kind} aria-hidden="true" />
          <span>{describeCoreStatus(status)}</span>
        </footer>
      </aside>

      <main className="workspace">
        {view === "models" ? (
          <ModelsView
            onClose={() => {
              setView("work");
            }}
          />
        ) : view === "apps" ? (
          <AppsView
            onClose={() => {
              setView("work");
            }}
          />
        ) : (
          <>
            <header className="workspace-header">
              <div>
                <span className="eyebrow">Repository work</span>
                <h1>
                  {selectedConversation?.title ?? inspection?.project.name ?? "Open a project"}
                </h1>
                <p>
                  {inspection?.project.roots[0] ??
                    "Choose a local directory to create a canonical, restart-safe project record."}
                </p>
              </div>
              <div
                className="runtime-chip"
                title="Inspect task activity for the selected worker, model and destination"
              >
                <span className="status-dot" data-state="ready" aria-hidden="true" />
                governed task runtime
              </div>
            </header>

            {failure !== undefined && (
              <div className="notice notice--error" role="alert">
                {failure}
              </div>
            )}

            {approvals[0] !== undefined && (
              <ApprovalCard
                key={approvals[0].approvalId}
                request={approvals[0]}
                waiting={approvals.length - 1}
                onAnswered={onApprovalAnswered}
              />
            )}

            {loadingProject ? (
              <p className="workspace-loading">Loading durable project record…</p>
            ) : inspection === undefined ? (
              <section className="empty-state" aria-labelledby="empty-heading">
                <div className="blueprint-corners" aria-hidden="true" />
                <span className="eyebrow">Local by default</span>
                <h2 id="empty-heading">Start with an approved project root.</h2>
                <p>
                  Tramontana stores project identity, conversations, task history, and configuration
                  manifests in the local profile. Markdown is parsed as configuration data and never
                  executed.
                </p>
                <button
                  type="button"
                  className="button button--primary"
                  onClick={onOpenProject}
                  disabled={status.kind !== "ready" || openingProject}
                >
                  Open a project folder
                </button>
              </section>
            ) : (
              <>
                <section className="task-strip" aria-labelledby="tasks-heading">
                  <div className="section-heading-row">
                    <h2 id="tasks-heading">Recent tasks</h2>
                    <span>{tasks.length} in this conversation</span>
                  </div>
                  {selectedConversation === undefined ? (
                    <p className="inline-empty">Create a conversation to begin durable work.</p>
                  ) : tasks.length === 0 ? (
                    <p className="inline-empty">No tasks yet. The first run will appear here.</p>
                  ) : (
                    <ul className="task-tabs" aria-label="Tasks in this conversation">
                      {tasks.map((task) => (
                        <li key={task.taskId}>
                          <button
                            type="button"
                            data-selected={selectedTaskId === task.taskId}
                            aria-current={selectedTaskId === task.taskId ? "true" : undefined}
                            onClick={() => void selectTask(task.taskId)}
                          >
                            <span data-task-state={task.state}>{task.state}</span>
                            <strong>{task.goalSummary}</strong>
                            <code>{shortId(task.taskId)}</code>
                          </button>
                        </li>
                      ))}
                    </ul>
                  )}
                </section>

                {selectedTask?.state === "interrupted" && selectedTaskId !== undefined && (
                  <CheckpointPanel
                    key={selectedTaskId}
                    taskId={selectedTaskId}
                    stoppedBecause={stoppedBecause(events)}
                    onChanged={() => {
                      if (selectedConversationId !== undefined) {
                        void selectConversation(selectedConversationId);
                      }
                    }}
                  />
                )}

                <section className="activity" aria-labelledby="activity-heading">
                  <div className="section-heading-row">
                    <h2 id="activity-heading">Activity</h2>
                    <span>
                      {selectedTask === undefined
                        ? "No selected task"
                        : `${shortId(selectedTask.taskId)} · ${events.length} durable events`}
                    </span>
                  </div>
                  {events.length === 0 ? (
                    <div className="activity-empty">
                      <span className="eyebrow">Sequence gap-free</span>
                      <p>Start a task to see its canonical event stream.</p>
                    </div>
                  ) : (
                    <>
                      <TaskHeader header={header} onRetry={onRetry} />
                      <ActivityCards cards={cards} />
                      {/* The record itself, one line per event, as the CLI replays it. */}
                      <details className="raw-events">
                        <summary>All events ({events.length})</summary>
                        <ol className="event-list" aria-label="Task event stream">
                          {events.map((event) => {
                            const line = describeEvent(event);
                            return (
                              <li key={event.eventId}>
                                <span className="event-sequence">{line.sequence}</span>
                                <code>{line.kind}</code>
                                <span>{line.detail}</span>
                              </li>
                            );
                          })}
                        </ol>
                      </details>
                    </>
                  )}
                  {stream !== undefined && stream.text !== "" && (
                    <div className="stream-output" role="region" aria-label="Live answer">
                      {/* A window that joined an answer already under way holds
                          only the end of it, and says so rather than letting a
                          fragment read as the whole thing. */}
                      <span>{stream.partial ? "live frame · joined part-way" : "live frame"}</span>
                      <p>{stream.text}</p>
                    </div>
                  )}
                </section>

                <form
                  className="composer"
                  onSubmit={(submitted) => {
                    submitted.preventDefault();
                    onStart();
                  }}
                >
                  <label htmlFor="goal">Task goal</label>
                  <textarea
                    ref={goalInput}
                    id="goal"
                    value={goal}
                    onChange={(changed) => {
                      setGoal(changed.target.value);
                    }}
                    disabled={running}
                    rows={2}
                  />
                  <fieldset className="composer-options" disabled={running}>
                    <legend className="sr-only">How this task may run</legend>
                    <fieldset className="composer-modes">
                      <legend className="sr-only">Where models may run</legend>
                      {MODES.map((mode) => (
                        <label key={mode} data-selected={routingMode === mode}>
                          <input
                            type="radio"
                            name="routing-mode"
                            value={mode}
                            checked={routingMode === mode}
                            onChange={() => {
                              setRoutingMode(mode);
                            }}
                          />
                          {MODE_LABEL[mode]}
                        </label>
                      ))}
                    </fieldset>
                    <label className="composer-toggle">
                      <input
                        type="checkbox"
                        checked={mayWrite}
                        onChange={(changed) => {
                          setMayWrite(changed.target.checked);
                        }}
                      />
                      May edit files
                    </label>
                    <label className="composer-toggle">
                      <input
                        type="checkbox"
                        checked={mayRunRecipes}
                        onChange={(changed) => {
                          setMayRunRecipes(changed.target.checked);
                        }}
                      />
                      May run recipes
                    </label>
                    <label className="composer-toggle">
                      <input
                        type="checkbox"
                        checked={mayUseApps}
                        onChange={(changed) => {
                          setMayUseApps(changed.target.checked);
                        }}
                      />
                      May use Apps
                    </label>
                    <label className="composer-toggle">
                      <input
                        type="checkbox"
                        checked={mayBrowse}
                        onChange={(changed) => {
                          setMayBrowse(changed.target.checked);
                        }}
                      />
                      May browse the web
                    </label>
                  </fieldset>
                  <p className="composer-disclosure">
                    {describeChoices(routingMode, mayWrite, mayRunRecipes, mayUseApps, mayBrowse)}
                  </p>
                  <div className="composer-footer">
                    <span>
                      {configuration === undefined
                        ? "No configuration manifest"
                        : `${configuration.sources.filter((source) => source.active).length} active sources · ${configuration.manifestHash.slice(0, 12)}`}
                    </span>
                    <button
                      type="button"
                      className="button button--secondary"
                      onClick={onCancel}
                      disabled={!running}
                    >
                      Stop
                    </button>
                    <button
                      type="submit"
                      className="button button--primary"
                      disabled={
                        running || selectedConversationId === undefined || goal.trim() === ""
                      }
                    >
                      Start task
                    </button>
                  </div>
                </form>
              </>
            )}
          </>
        )}
      </main>

      <aside className="inspector" aria-label="Project inspector">
        <div className="inspector-header">
          <span className="eyebrow">Effective configuration</span>
          <h2>Instruction manifest</h2>
          <code>{configuration?.manifestHash ?? "No manifest resolved"}</code>
        </div>
        {configuration === undefined ? (
          <p className="inspector-empty">Open a project to inspect its resolved sources.</p>
        ) : (
          <>
            <dl className="manifest-stats">
              <div>
                <dt>Discovered</dt>
                <dd>{configuration.sources.length}</dd>
              </div>
              <div>
                <dt>Active</dt>
                <dd>{configuration.sources.filter((source) => source.active).length}</dd>
              </div>
              <div>
                <dt>Diagnostics</dt>
                <dd>{configuration.diagnostics.length}</dd>
              </div>
            </dl>
            {configuration.diagnostics.length > 0 && (
              <ul className="diagnostic-list" aria-label="Configuration diagnostics">
                {configuration.diagnostics.map((diagnostic, index) => (
                  <li
                    key={`${diagnostic.relativePath}:${index}`}
                    data-severity={diagnostic.severity}
                  >
                    {diagnostic.severity === "error" ? "Error" : "Warning"} in{" "}
                    <code>{diagnostic.relativePath}</code>: {diagnostic.message}
                  </li>
                ))}
              </ul>
            )}
            <ol className="source-list">
              {configuration.sources.map((source, index) => (
                <li key={source.sourceId} data-active={source.active}>
                  <span className="source-rank">{String(index + 1).padStart(2, "0")}</span>
                  <div>
                    <code>{source.relativePath}</code>
                    <span>{source.kind}</span>
                  </div>
                  <span className="source-state">
                    {!source.enabled ? "disabled" : source.active ? "active" : "inert"}
                  </span>
                </li>
              ))}
            </ol>
            <p className="manifest-note">
              Bodies stay in the project. This profile retains source metadata and hashes; task
              events capture the secret-free manifest used at start.
            </p>
          </>
        )}
        {inspection !== undefined && (
          <>
            <ChangesPanel projectId={inspection.project.projectId} revision={revision} />
            <GrantsPanel projectId={inspection.project.projectId} revision={revision} />
          </>
        )}
        {/* Outside the project: a new profile has nothing open, and importing
            a bundle is how it gets something. */}
        <BundlesPanel
          projectId={inspection?.project.projectId}
          taskId={selectedTaskId}
          onImported={() => {
            void listProjects().then((records) => {
              setProjects(records);
              // Into an empty profile, open what was just imported.
              const first = records[0];
              if (inspection === undefined && first !== undefined) {
                void selectProject(first.projectId);
              }
            }, undefined);
          }}
        />
      </aside>
    </div>
  );
}
