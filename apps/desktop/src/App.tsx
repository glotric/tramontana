import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isSchemaCompatible, SCHEMA_VERSION } from "@tramontana/shared";
import type {
  ApprovalRequest,
  ConversationRecord,
  EventEnvelope,
  ProjectRecord,
} from "@tramontana/shared";

import { isTerminal, projectState } from "./activity.js";
import { AgentComposer } from "./AgentComposer.js";
import { AgentHome } from "./AgentHome.js";
import { ApprovalCard } from "./ApprovalCard.js";
import { useAttachments } from "./Attachments.js";
import { BundlesPanel } from "./BundlesPanel.js";
import { buildCards, describeHeader, MODE_LABEL } from "./cards.js";
import { ChangesPanel } from "./ChangesPanel.js";
import { ChatView } from "./ChatView.js";
import { CheckpointPanel } from "./CheckpointPanel.js";
import { titleFromGoal } from "./choices.js";
import { ConversationMenu } from "./ConversationMenu.js";
import { CustomizeSettings } from "./CustomizeSettings.js";
import {
  cancelTask,
  createConversation,
  deleteProject,
  failureText,
  fetchCoreInfo,
  listConversations,
  listPendingApprovals,
  listProjects,
  onApprovalRequested,
  openProject,
  personalProfile,
  startTask,
} from "./core.js";
import { appendDictated, useDictation } from "./Dictation.js";
import { Glyph } from "./Glyph.js";
import { GrantsPanel } from "./GrantsPanel.js";
import { Icon } from "./icons.js";
import { ModelsView } from "./ModelsView.js";
import { ProjectsView } from "./ProjectsView.js";
import { ScheduledView } from "./ScheduledView.js";
import { SearchPanel } from "./SearchPanel.js";
import { SettingsView } from "./SettingsView.js";
import type { SettingsTab } from "./SettingsView.js";
import { ShareDialog } from "./ShareDialog.js";
import { Sidebar } from "./Sidebar.js";
import type { Mode, View } from "./Sidebar.js";
import { TaskThread } from "./TaskThread.js";
import { CommandTerminal } from "./CommandTerminal.js";
import { commandRuns, commandUnderway } from "./terminal.js";
import { usePersonalSkills, useSkillSlash } from "./SkillSlash.js";
import type { SkillOption } from "./SkillSlash.js";
import { useComposerOptions } from "./useComposerOptions.js";
import { useProjectSelection } from "./useProjectSelection.js";
import { useTaskStream } from "./useTaskStream.js";

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

/** The attachments option, only when there are files to send. */
function attached(tokens: readonly string[]): { readonly attachments?: readonly string[] } {
  return tokens.length === 0 ? {} : { attachments: tokens };
}

/**
 * On macOS the window has no title bar: its traffic lights sit over the
 * sidebar, and the sidebar and header leave room for them.
 */
const MAC = typeof navigator !== "undefined" && navigator.platform.includes("Mac");

/** Where the window remembers which half it was last showing. */
const MODE_KEY = "tramontana.mode";

/** The half the window last showed, or Chat, as the design starts. */
function rememberedMode(): Mode {
  try {
    return localStorage.getItem(MODE_KEY) === "agent" ? "agent" : "chat";
  } catch {
    return "chat";
  }
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
  const [goal, setGoal] = useState("");
  const [approvals, setApprovals] = useState<readonly ApprovalRequest[]>([]);
  // Moves on whenever a task ends, so the Changes panel reads the tree again.
  const [revision, setRevision] = useState(0);
  // The work area, or the models and credentials the tasks run against.
  const [view, setView] = useState<View>("work");
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  // Both side panels can be put away, leaving the work column on its own.
  // The details start put away, every time: the work comes first.
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [detailsOpen, setDetailsOpen] = useState(false);
  // What the person asked to be called (ADR 0061), for the greeting and the
  // sidebar. Read again on leaving Settings, where it is changed.
  const [personName, setPersonName] = useState<string | undefined>();
  // Chat for questions and writing, Agent for work in a project (ADR 0057).
  // Which one the window opens in is a convenience kept in this browser
  // profile, not a record.
  const [mode, setMode] = useState<Mode>(rememberedMode);
  const [chats, setChats] = useState<readonly ConversationRecord[]>([]);
  const [selectedChatId, setSelectedChatId] = useState<string | undefined>();
  const [searchOpen, setSearchOpen] = useState(false);
  // The conversation being shared, while its dialog is open (ADR 0060).
  const [sharing, setSharing] = useState<ConversationRecord | undefined>();

  // ⌘K on macOS, Ctrl+K elsewhere, as the design's sidebar says.
  useEffect(() => {
    const onKey = (pressed: KeyboardEvent): void => {
      if ((pressed.metaKey || pressed.ctrlKey) && pressed.key.toLowerCase() === "k") {
        pressed.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, []);
  const [failure, setFailure] = useState<string | undefined>();
  const [openingProject, setOpeningProject] = useState(false);

  // What the window does with each new durable event of the task on screen.
  // Set below, once the project state it touches exists.
  const onTaskEvent = useRef<(event: EventEnvelope) => void>(() => undefined);
  const {
    selectedTaskId,
    events,
    stream,
    select: selectTask,
    begin: beginTask,
    adopt: adoptTask,
    clear: clearTask,
  } = useTaskStream(onTaskEvent, setFailure);
  const {
    projects,
    setProjects,
    inspection,
    setInspection,
    conversations,
    setConversations,
    selectedConversationId,
    tasks,
    loadingProject,
    selectProject,
    selectConversation,
    closeProject,
    leaveConversation,
    addConversation,
    refreshTasks,
  } = useProjectSelection({ select: selectTask, clear: clearTask }, setFailure);
  const composer = useComposerOptions(goal, inspection?.project.projectId);
  const { routingMode, setRoutingMode, admission, chosenWorker } = composer;
  // Skills come by `/` (ADR 0070): the person's own, and the project's.
  const personalSkills = usePersonalSkills();
  const projectSkillOptions = useMemo<readonly SkillOption[]>(
    () =>
      composer.listedSkills.map((skill) => ({
        kind: "project",
        id: skill.id,
        name: skill.displayName,
        ...(skill.description === undefined ? {} : { description: skill.description }),
        suggested: composer.suggested.includes(skill.id),
      })),
    [composer.listedSkills, composer.suggested],
  );
  const slash = useSkillSlash({
    draft: goal,
    setDraft: setGoal,
    personal: personalSkills.skills,
    project: projectSkillOptions,
    onOpen: personalSkills.refresh,
  });
  // Only the open project's own skills can reach its task.
  const chosenSkills = slash.projectIds.filter((id) =>
    composer.offeredSkills.some((skill) => skill.id === id),
  );
  useEffect(() => {
    onTaskEvent.current = (event) => {
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
        setRevision((current) => current + 1);
      }
    };
  }, [refreshTasks, setInspection]);

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
    if (status.kind !== "ready" || view === "settings") return undefined;
    let active = true;
    void personalProfile().then(
      (profile) => {
        if (active) setPersonName(profile.name);
      },
      () => undefined,
    );
    return () => {
      active = false;
    };
  }, [status.kind, view]);

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

  const refreshChats = useCallback(() => {
    void listConversations(undefined).then(setChats, () => undefined);
  }, []);
  useEffect(() => {
    if (status.kind === "ready") refreshChats();
  }, [status.kind, refreshChats]);
  const selectedChat = useMemo(
    () => chats.find((chat) => chat.conversationId === selectedChatId),
    [chats, selectedChatId],
  );
  const chooseMode = useCallback((next: Mode) => {
    setMode(next);
    setView("work");
    try {
      localStorage.setItem(MODE_KEY, next);
    } catch {
      // A window that cannot remember still switches.
    }
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
  // The commands this task ran, for the terminal (it pops up on a new one).
  const runs = useMemo(() => commandRuns(events), [events]);
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [runsSeen, setRunsSeen] = useState(0);
  if (runs.length !== runsSeen) {
    setRunsSeen(runs.length);
    if (runs.length > runsSeen && commandUnderway(runs)) setTerminalOpen(true);
    if (runs.length === 0) setTerminalOpen(false);
  }
  const header = useMemo(() => describeHeader(events), [events]);
  const goalInput = useRef<HTMLTextAreaElement>(null);
  const agentFiles = useAttachments();
  const agentDictation = useDictation((text) => {
    setGoal((current) => appendDictated(current, text));
  });

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
  }, [selectProject, setProjects]);

  /**
   * Remove a project from Tramontana; its folder is not touched. When it was
   * the one open, the next project opens in its place, so the composer is
   * never left without one while others remain.
   */
  const removeProject = useCallback(
    async (project: ProjectRecord): Promise<void> => {
      await deleteProject(project.projectId);
      const remaining = await listProjects();
      setProjects(remaining);
      if (inspection?.project.projectId !== project.projectId) return;
      const next = remaining[0];
      if (next === undefined) closeProject();
      else await selectProject(next.projectId);
    },
    [closeProject, inspection?.project.projectId, selectProject, setProjects],
  );

  const onStart = useCallback(() => {
    const projectId = inspection?.project.projectId;
    if (projectId === undefined) {
      setFailure("Open a project before starting a task.");
      return;
    }
    if (goal.trim() === "") return;
    setFailure(undefined);
    const tokens = agentFiles.tokens();
    const options = {
      ...admission,
      routingMode,
      ...(chosenWorker === undefined ? {} : { worker: chosenWorker.agentId }),
      ...(chosenSkills.length === 0 ? {} : { skills: chosenSkills }),
      ...(slash.personalIds.length === 0 ? {} : { personalSkills: slash.personalIds }),
      ...attached(tokens),
    };

    // From the home screen, the task starts a conversation of its own,
    // named after what it was asked; it can be renamed afterwards.
    const conversation =
      selectedConversationId === undefined
        ? createConversation(projectId, titleFromGoal(goal)).then((created) => {
            addConversation(created);
            return created.conversationId;
          })
        : Promise.resolve(selectedConversationId);

    void conversation
      .then((conversationId) => {
        // A new task on screen: updates from any stream opened for the one
        // before it are no longer this window's to show.
        const onUpdate = beginTask();
        return startTask(goal.trim(), onUpdate, conversationId, options).then((id) => {
          // Only now: a task that failed to start keeps its files.
          agentFiles.sent(tokens);
          setGoal("");
          // The skills went with this message, as Claude's do.
          slash.clear();
          adoptTask(id);
          refreshTasks(conversationId);
        });
      })
      .catch((error: unknown) => {
        setFailure(failureText(error, "Could not start the task."));
      });
  }, [
    addConversation,
    adoptTask,
    admission,
    agentFiles,
    beginTask,
    chosenSkills,
    slash,
    chosenWorker,
    goal,
    inspection?.project.projectId,
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
    <div
      className="app-shell"
      data-platform={MAC ? "mac" : "other"}
      data-sidebar={sidebarOpen ? "open" : "closed"}
      data-details={detailsOpen && view === "work" && mode === "agent" ? "open" : "closed"}
    >
      {sharing !== undefined && (
        <ShareDialog
          conversation={sharing}
          onClose={() => {
            setSharing(undefined);
          }}
        />
      )}
      {searchOpen && (
        <SearchPanel
          onClose={() => {
            setSearchOpen(false);
          }}
          onOpenConversation={(hit) => {
            setSearchOpen(false);
            const projectId = hit.conversation.projectId;
            if (projectId === undefined) {
              chooseMode("chat");
              refreshChats();
              setSelectedChatId(hit.conversation.conversationId);
            } else {
              chooseMode("agent");
              void selectProject(projectId).then(() =>
                selectConversation(hit.conversation.conversationId),
              );
            }
          }}
          onOpenProject={(project) => {
            setSearchOpen(false);
            chooseMode("agent");
            void selectProject(project.projectId);
          }}
        />
      )}
      {sidebarOpen && (
        <Sidebar
          mode={mode}
          view={view}
          onView={(next) => {
            setView((current) => (current === next ? "work" : next));
          }}
          onHide={() => {
            setSidebarOpen(false);
          }}
          onSearch={() => {
            setSearchOpen(true);
          }}
          onNew={() => {
            setView("work");
            if (mode === "chat") setSelectedChatId(undefined);
            else leaveConversation();
          }}
          chats={chats}
          selectedChatId={selectedChatId}
          onSelectChat={(conversationId) => {
            setSelectedChatId(conversationId);
            setView("work");
          }}
          projects={projects}
          openProjectId={inspection?.project.projectId}
          conversations={conversations}
          selectedConversationId={selectedConversationId}
          onOpenConversation={(projectId, conversationId) => {
            setView("work");
            if (projectId === inspection?.project.projectId) {
              void selectConversation(conversationId);
            } else {
              void selectProject(projectId).then(() => selectConversation(conversationId));
            }
          }}
          onNewTaskIn={(projectId) => {
            setView("work");
            if (projectId === inspection?.project.projectId) leaveConversation();
            else void selectProject(projectId);
            goalInput.current?.focus();
          }}
          onOpenProject={onOpenProject}
          onRemoveProject={removeProject}
          openingProject={openingProject}
          canOpenProject={status.kind === "ready"}
          personName={personName}
          status={status.kind}
          statusText={describeCoreStatus(status)}
        />
      )}

      <main className="workspace">
        {/* Without a title bar, the top of the window still moves it: a strip
            above everything, taller on the screens with no header (ADR 0069). */}
        <div
          className="window-drag"
          data-tall={view !== "work" ? "true" : undefined}
          data-tauri-drag-region
          aria-hidden="true"
        />
        {/* The views without a work header (Scheduled, Projects, Settings,
            Models, Apps) still need a way back to the sidebar. */}
        {!sidebarOpen && view !== "work" && (
          <button
            type="button"
            className="icon-button sidebar-restore"
            aria-label="Show sidebar"
            onClick={() => {
              setSidebarOpen(true);
            }}
          >
            <Icon name="sidebar" />
          </button>
        )}
        {view === "scheduled" ? (
          <ScheduledView
            projects={projects}
            onOpen={(schedule) => {
              if (schedule.projectId === undefined) {
                chooseMode("chat");
                refreshChats();
                setSelectedChatId(schedule.conversationId);
              } else {
                chooseMode("agent");
                void selectProject(schedule.projectId).then(() =>
                  selectConversation(schedule.conversationId),
                );
              }
            }}
          />
        ) : view === "settings" ? (
          <SettingsView
            tab={settingsTab}
            onTab={setSettingsTab}
            customize={<CustomizeSettings />}
            models={<ModelsView />}
          />
        ) : view === "projects" ? (
          <ProjectsView
            projects={projects}
            opening={openingProject}
            onNew={() => {
              setView("work");
              onOpenProject();
            }}
            onOpen={(project) => {
              setView("work");
              chooseMode("agent");
              void selectProject(project.projectId);
            }}
            onDelete={removeProject}
          />
        ) : mode === "chat" ? (
          <>
            <header className="workspace-header" data-tauri-drag-region>
              {!sidebarOpen && (
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Show sidebar"
                  onClick={() => {
                    setSidebarOpen(true);
                  }}
                >
                  <Icon name="sidebar" />
                </button>
              )}
              <div className="workspace-title workspace-title--row" data-tauri-drag-region>
                <h1 data-tauri-drag-region>{selectedChat?.title ?? "New chat"}</h1>
                {selectedChat !== undefined && (
                  <ConversationMenu
                    key={selectedChat.conversationId}
                    conversation={selectedChat}
                    onRenamed={(renamed) => {
                      setChats((current) =>
                        current.map((chat) =>
                          chat.conversationId === renamed.conversationId ? renamed : chat,
                        ),
                      );
                    }}
                    onDeleted={(conversationId) => {
                      setChats((current) =>
                        current.filter((chat) => chat.conversationId !== conversationId),
                      );
                      setSelectedChatId(undefined);
                    }}
                  />
                )}
              </div>
              <ModeSwitch mode={mode} onChange={chooseMode} />
              <div className="workspace-header-end" data-tauri-drag-region>
                <span className="runtime-chip" data-mode={routingMode}>
                  {routingMode === "localOnly" && <Icon name="lock" size={13} strokeWidth={2.2} />}
                  {MODE_LABEL[routingMode]}
                </span>
                {selectedChat !== undefined && (
                  <button
                    type="button"
                    className="pill-button"
                    onClick={() => {
                      setSharing(selectedChat);
                    }}
                  >
                    <Icon name="share" size={15} />
                    Share
                  </button>
                )}
              </div>
            </header>
            <ChatView
              conversation={selectedChat}
              routingMode={routingMode}
              onRoutingMode={setRoutingMode}
              onCreated={(created) => {
                setChats((current) => [created, ...current]);
                setSelectedChatId(created.conversationId);
              }}
              onTurnEnded={refreshChats}
              notice={
                approvals[0] !== undefined && (
                  <ApprovalCard
                    key={approvals[0].approvalId}
                    request={approvals[0]}
                    waiting={approvals.length - 1}
                    onAnswered={onApprovalAnswered}
                  />
                )
              }
            />
          </>
        ) : (
          <>
            <header className="workspace-header" data-tauri-drag-region>
              {!sidebarOpen && (
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Show sidebar"
                  onClick={() => {
                    setSidebarOpen(true);
                  }}
                >
                  <Icon name="sidebar" />
                </button>
              )}
              <div className="workspace-title workspace-title--row" data-tauri-drag-region>
                <h1 data-tauri-drag-region>{selectedConversation?.title ?? "New task"}</h1>
                {selectedConversation !== undefined && inspection !== undefined && (
                  <ConversationMenu
                    key={selectedConversation.conversationId}
                    conversation={selectedConversation}
                    onRenamed={(renamed) => {
                      setConversations((current) =>
                        current.map((conversation) =>
                          conversation.conversationId === renamed.conversationId
                            ? renamed
                            : conversation,
                        ),
                      );
                    }}
                    onDeleted={() => {
                      void selectProject(inspection.project.projectId);
                    }}
                  />
                )}
              </div>
              <ModeSwitch mode={mode} onChange={chooseMode} />
              <div className="workspace-header-end" data-tauri-drag-region>
                {selectedConversation !== undefined && (
                  <button
                    type="button"
                    className="pill-button"
                    onClick={() => {
                      setSharing(selectedConversation);
                    }}
                  >
                    <Icon name="share" size={15} />
                    Share
                  </button>
                )}
                {runs.length > 0 && !terminalOpen && (
                  <button
                    type="button"
                    className="icon-button"
                    aria-label="Terminal"
                    title="The commands this task ran"
                    onClick={() => {
                      setTerminalOpen(true);
                    }}
                  >
                    <Icon name="terminal" />
                  </button>
                )}
                {/* Only while the panel is put away: once open, it closes from
                  its own button. */}
                {!detailsOpen && (
                  <button
                    type="button"
                    className="icon-button"
                    aria-label="Details"
                    title="Details: the project's configuration, changes and permissions"
                    onClick={() => {
                      setDetailsOpen(true);
                    }}
                  >
                    <Icon name="panel" />
                  </button>
                )}
              </div>
            </header>

            <div className="workspace-scroll">
              <div className="workspace-column">
                {failure !== undefined && (
                  <div className="notice notice--error" role="alert">
                    {failure}
                  </div>
                )}
                {agentFiles.error !== undefined && (
                  <div className="notice notice--error" role="alert">
                    {agentFiles.error}
                  </div>
                )}
                {agentDictation.error !== undefined && (
                  <div className="notice notice--error" role="alert">
                    {agentDictation.error}
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
                  <p className="workspace-loading">Loading the project…</p>
                ) : selectedConversation === undefined ? (
                  <AgentHome
                    name={personName}
                    projects={projects}
                    revision={revision}
                    onOpenProject={onOpenProject}
                    canOpenProject={status.kind === "ready" && !openingProject}
                  />
                ) : (
                  <TaskThread
                    tasks={tasks}
                    selectedTaskId={selectedTaskId}
                    onSelectTask={(taskId) => void selectTask(taskId)}
                    header={header}
                    cards={cards}
                    events={events}
                    stream={stream}
                    running={running}
                    onRetry={onRetry}
                    interruption={
                      selectedTask?.state === "interrupted" &&
                      selectedTaskId !== undefined && (
                        <CheckpointPanel
                          key={selectedTaskId}
                          taskId={selectedTaskId}
                          stoppedBecause={stoppedBecause(events)}
                          onChanged={() => {
                            void selectConversation(selectedConversation.conversationId);
                          }}
                        />
                      )
                    }
                  />
                )}
              </div>
            </div>

            {terminalOpen && runs.length > 0 && (
              <CommandTerminal
                runs={runs}
                onClose={() => {
                  setTerminalOpen(false);
                }}
              />
            )}

            {!loadingProject && (
              <AgentComposer
                goal={goal}
                onGoal={setGoal}
                goalInput={goalInput}
                options={composer}
                projects={projects}
                project={inspection?.project}
                onChooseProject={(projectId) => {
                  if (projectId !== inspection?.project.projectId) void selectProject(projectId);
                }}
                onOpenProject={onOpenProject}
                files={agentFiles}
                dictation={agentDictation}
                running={running}
                canStart={inspection !== undefined && goal.trim() !== ""}
                onStart={onStart}
                onCancel={onCancel}
                slash={slash}
                hasSkills={personalSkills.skills.length > 0 || projectSkillOptions.length > 0}
              />
            )}
          </>
        )}
      </main>

      <aside
        className="inspector"
        aria-label="Project inspector"
        hidden={!detailsOpen || view !== "work" || mode !== "agent"}
      >
        <div className="inspector-header">
          <button
            type="button"
            className="close-button"
            aria-label="Close details"
            onClick={() => {
              setDetailsOpen(false);
            }}
          >
            <Icon name="close" size={14} strokeWidth={2.2} />
          </button>
          {inspection !== undefined && (
            <p className="inspector-project">
              <strong>{inspection.project.name}</strong>
              <code>{inspection.project.roots[0]}</code>
            </p>
          )}
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

/** The design's Chat / Agent switch: one of two, said in words. */
function ModeSwitch({
  mode,
  onChange,
}: {
  readonly mode: Mode;
  readonly onChange: (mode: Mode) => void;
}): React.JSX.Element {
  return (
    <div className="mode-switch" role="group" aria-label="Mode">
      <button
        type="button"
        aria-pressed={mode === "chat"}
        onClick={() => {
          onChange("chat");
        }}
      >
        <Glyph icon="chat" hue="blue" size={24} />
        Chat
      </button>
      <button
        type="button"
        aria-pressed={mode === "agent"}
        onClick={() => {
          onChange("agent");
        }}
      >
        <Glyph icon="agent" hue="purple" size={24} />
        Agent
      </button>
    </div>
  );
}
