import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isSchemaCompatible, SCHEMA_VERSION } from "@tramontana/shared";
import type {
  ApprovalRequest,
  ConversationRecord,
  EventEnvelope,
  ProjectRecord,
  TaskRecord,
  TaskUpdate,
} from "@tramontana/shared";

import { describeEvent, isTerminal, projectState } from "./activity.js";
import { ApprovalCard } from "./ApprovalCard.js";
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
  const [streamed, setStreamed] = useState("");
  const [goal, setGoal] = useState("Say hello");
  const [conversationTitle, setConversationTitle] = useState("");
  const [approvals, setApprovals] = useState<readonly ApprovalRequest[]>([]);
  const [failure, setFailure] = useState<string | undefined>();
  const [loadingProject, setLoadingProject] = useState(false);
  const [openingProject, setOpeningProject] = useState(false);

  const projectRequest = useRef(0);
  const conversationRequest = useRef(0);
  const taskRequest = useRef(0);
  const seen = useRef(new Set<number>());

  const selectTask = useCallback(async (taskId: string): Promise<void> => {
    const request = ++taskRequest.current;
    setSelectedTaskId(taskId);
    setStreamed("");
    setFailure(undefined);
    try {
      const replayed = await replayTaskEvents(taskId);
      if (request !== taskRequest.current) return;
      seen.current = new Set(replayed.map((event) => event.sequence));
      setEvents(replayed);
    } catch (error: unknown) {
      if (request === taskRequest.current) {
        setFailure(failureText(error, "Could not replay the task history."));
      }
    }
  }, []);

  const selectConversation = useCallback(
    async (conversationId: string): Promise<void> => {
      const request = ++conversationRequest.current;
      taskRequest.current += 1;
      setSelectedConversationId(conversationId);
      setSelectedTaskId(undefined);
      setTasks([]);
      setEvents([]);
      setStreamed("");
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
      setStreamed("");
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

  const onStart = useCallback(() => {
    if (selectedConversationId === undefined) {
      setFailure("Create or select a conversation before starting a task.");
      return;
    }
    seen.current = new Set();
    setEvents([]);
    setStreamed("");
    setFailure(undefined);
    setSelectedTaskId(undefined);

    const onUpdate = (update: TaskUpdate): void => {
      switch (update.kind) {
        case "event": {
          const { event } = update;
          if (seen.current.has(event.sequence)) return;
          seen.current.add(event.sequence);
          setEvents((previous) => [...previous, event]);
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
            refreshTasks();
          }
          break;
        }
        case "frame":
          setStreamed((previous) => previous + update.frame.text);
          break;
      }
    };

    void startTask(goal, onUpdate, selectedConversationId).then(
      (id) => {
        setSelectedTaskId(id);
        refreshTasks();
      },
      (error: unknown) => {
        setFailure(failureText(error, "Could not start the task."));
      },
    );
  }, [goal, refreshTasks, selectedConversationId]);

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
                      onClick={() => void selectProject(project.projectId)}
                    >
                      <span className="status-dot" />
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

        <footer className="rail-status">
          <span className="status-dot" data-state={status.kind} />
          <span>{describeCoreStatus(status)}</span>
        </footer>
      </aside>

      <main className="workspace">
        <header className="workspace-header">
          <div>
            <span className="eyebrow">M6 · Read-only repository work</span>
            <h1>{selectedConversation?.title ?? inspection?.project.name ?? "Open a project"}</h1>
            <p>
              {inspection?.project.roots[0] ??
                "Choose a local directory to create a canonical, restart-safe project record."}
            </p>
          </div>
          <div
            className="runtime-chip"
            title="Inspect task activity for the selected worker, model and destination"
          >
            <span className="status-dot" data-state="ready" />
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
                <div className="task-tabs" role="list">
                  {tasks.map((task) => (
                    <button
                      type="button"
                      role="listitem"
                      key={task.taskId}
                      data-selected={selectedTaskId === task.taskId}
                      onClick={() => void selectTask(task.taskId)}
                    >
                      <span data-task-state={task.state}>{task.state}</span>
                      <strong>{task.goalSummary}</strong>
                      <code>{shortId(task.taskId)}</code>
                    </button>
                  ))}
                </div>
              )}
            </section>

            {selectedTask?.state === "interrupted" && selectedTaskId !== undefined && (
              <CheckpointPanel
                key={selectedTaskId}
                taskId={selectedTaskId}
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
              )}
              {streamed !== "" && (
                <div className="stream-output">
                  <span>live frame</span>
                  <p>{streamed}</p>
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
                id="goal"
                value={goal}
                onChange={(changed) => {
                  setGoal(changed.target.value);
                }}
                disabled={running}
                rows={2}
              />
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
                  disabled={running || selectedConversationId === undefined || goal.trim() === ""}
                >
                  Start task
                </button>
              </div>
            </form>
          </>
        )}
      </main>

      <aside className="inspector" aria-label="Effective configuration">
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
      </aside>
    </div>
  );
}
