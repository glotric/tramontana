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
import { ChatView } from "./ChatView.js";
import { ConversationMenu } from "./ConversationMenu.js";
import { SearchPanel } from "./SearchPanel.js";
import { ScheduledView } from "./ScheduledView.js";
import { SettingsView } from "./SettingsView.js";
import { ShareDialog } from "./ShareDialog.js";
import { AttachButton, PendingFiles, useAttachments } from "./Attachments.js";
import { Glyph } from "./Glyph.js";
import { GrantsPanel } from "./GrantsPanel.js";
import { AppsView } from "./AppsView.js";
import { ModelsView } from "./ModelsView.js";
import { ProjectsView } from "./ProjectsView.js";
import type { SkillView, WorkerView } from "./core.js";
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
  listSkills,
  listWorkers,
  suggestSkills,
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

/** How long the typing settles before the skills are suggested again. */
const SUGGESTION_DELAY_MS = 300;

/** The attachments option, only when there are files to send. */
function attached(tokens: readonly string[]): { readonly attachments?: readonly string[] } {
  return tokens.length === 0 ? {} : { attachments: tokens };
}

/** The search shortcut as this platform writes it. */
const SHORTCUT =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘K" : "Ctrl K";

/** Chat or Agent: which half of the window is showing (ADR 0057). */
type Mode = "chat" | "agent";

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
  skills: number,
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
  // A skill is guidance the model is given, never a capability, so it is
  // said apart from what the task may ask to do (ADR 0053).
  const chosen =
    skills === 0
      ? ""
      : skills === 1
        ? " It is given one of the project's skills to follow."
        : ` It is given ${skills} of the project's skills to follow.`;
  return `${MODE_TEXT[mode]} ${access}${commands}${browsing}${chosen}`;
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
  const [workers, setWorkers] = useState<readonly WorkerView[]>([]);
  const [worker, setWorker] = useState<string>("");
  const [skills, setSkills] = useState<
    { readonly projectId: string; readonly skills: readonly SkillView[] } | undefined
  >();
  const [ticked, setTicked] = useState<readonly string[]>([]);
  const [suggested, setSuggested] = useState<readonly string[]>([]);
  const [conversationTitle, setConversationTitle] = useState("");
  const [approvals, setApprovals] = useState<readonly ApprovalRequest[]>([]);
  // Moves on whenever a task ends, so the Changes panel reads the tree again.
  const [revision, setRevision] = useState(0);
  // The work area, or the models and credentials the tasks run against.
  const [view, setView] = useState<
    "work" | "models" | "apps" | "projects" | "settings" | "scheduled"
  >("work");
  // Both side panels can be put away, leaving the work column on its own.
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [detailsOpen, setDetailsOpen] = useState(true);
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

  // The workers a task can run on (ADR 0048). Without any, the composer
  // shows no choice and every task runs the built-in worker.
  useEffect(() => {
    let active = true;
    void listWorkers().then(
      (found) => {
        if (active) setWorkers(found);
      },
      () => undefined,
    );
    return () => {
      active = false;
    };
  }, []);

  // The skills this project offers (ADR 0053). Held with the project they
  // came from, so another project's list is never shown while the new one
  // loads, and so changing project needs no reset.
  const skillProjectId = inspection?.project.projectId;
  useEffect(() => {
    if (skillProjectId === undefined) return;
    let active = true;
    void listSkills(skillProjectId).then(
      (found) => {
        if (active) setSkills({ projectId: skillProjectId, skills: found });
      },
      () => {
        if (active) setSkills({ projectId: skillProjectId, skills: [] });
      },
    );
    return () => {
      active = false;
    };
  }, [skillProjectId]);

  // Which of them look relevant to the goal as it stands (ADR 0054). The
  // rule is the core's, asked again when the typing settles rather than on
  // every keystroke. A suggestion marks and orders; it never ticks.
  useEffect(() => {
    if (skillProjectId === undefined) return;
    let active = true;
    const timer = setTimeout(() => {
      void suggestSkills(skillProjectId, goal).then(
        (found) => {
          if (active) setSuggested(found);
        },
        () => {
          if (active) setSuggested([]);
        },
      );
    }, SUGGESTION_DELAY_MS);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [skillProjectId, goal]);

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
  const header = useMemo(() => describeHeader(events), [events]);
  const goalInput = useRef<HTMLTextAreaElement>(null);
  const agentFiles = useAttachments();

  // Retry puts the goal back where the person chooses again; it starts nothing.
  const onRetry = useCallback((retried: string) => {
    setGoal(retried);
    goalInput.current?.focus();
  }, []);
  // A task runs on the worker the person picked. Under Local Only a model
  // off this device is not offered, because the core would refuse it
  // (ADR 0048).
  const offeredWorkers = useMemo(
    () => workers.filter((choice) => !(routingMode === "localOnly" && choice.cloud === true)),
    [routingMode, workers],
  );
  const chosenWorker =
    offeredWorkers.find((choice) => choice.agentId === worker) ?? offeredWorkers[0];
  // Offering is not selecting, and a tick belongs to the project it was made
  // in: only skills this project offers can reach a task, so changing project
  // drops the rest without anything having to clear them.
  const offeredSkills = useMemo(
    () => (skills !== undefined && skills.projectId === skillProjectId ? skills.skills : []),
    [skills, skillProjectId],
  );
  const chosenSkills = useMemo(
    () => ticked.filter((id) => offeredSkills.some((skill) => skill.id === id)),
    [ticked, offeredSkills],
  );
  // Suggested skills first, each in the order the core gave, then the rest in
  // the project's own order. Stable, so a list does not reshuffle under the
  // pointer as the goal is typed.
  const listedSkills = useMemo(
    () => [
      ...offeredSkills.filter((skill) => suggested.includes(skill.id)),
      ...offeredSkills.filter((skill) => !suggested.includes(skill.id)),
    ],
    [offeredSkills, suggested],
  );

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
      ...(chosenWorker === undefined ? {} : { worker: chosenWorker.agentId }),
      ...(chosenSkills.length === 0 ? {} : { skills: chosenSkills }),
      ...attached(agentFiles.take()),
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
    agentFiles,
    chosenSkills,
    chosenWorker,
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
    <div
      className="app-shell"
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
        <aside className="rail" aria-label="Projects and conversations">
          <div className="brand-lockup">
            <Glyph icon="wind" hue="blue" size={34} />
            <strong>Tramontana</strong>
            <button
              type="button"
              className="icon-button"
              aria-label="Hide sidebar"
              onClick={() => {
                setSidebarOpen(false);
              }}
            >
              <SidebarIcon />
            </button>
          </div>

          {mode === "chat" ? (
            <button
              type="button"
              className="rail-row rail-row--nav"
              aria-pressed={view === "work" && selectedChatId === undefined}
              onClick={() => {
                setSelectedChatId(undefined);
                setView("work");
              }}
            >
              <Glyph icon="pen" hue="blue" size={28} />
              <span>New chat</span>
            </button>
          ) : (
            <>
              <button
                type="button"
                className="rail-row rail-row--nav"
                onClick={onOpenProject}
                disabled={status.kind !== "ready" || openingProject}
              >
                <Glyph icon="plus" hue="teal" size={28} />
                <span>{openingProject ? "Opening…" : "Open project"}</span>
              </button>
              <button
                type="button"
                className="rail-row rail-row--nav"
                aria-pressed={view === "projects"}
                onClick={() => {
                  setView((current) => (current === "projects" ? "work" : "projects"));
                }}
              >
                <Glyph icon="folder" hue="yellow" size={28} />
                <span>Projects</span>
              </button>
            </>
          )}
          <button
            type="button"
            className="rail-row rail-row--nav"
            aria-haspopup="dialog"
            onClick={() => {
              setSearchOpen(true);
            }}
          >
            <Glyph icon="search" hue="purple" size={28} />
            <span className="rail-row-label">Search</span>
            <kbd className="rail-shortcut" aria-hidden="true">
              {SHORTCUT}
            </kbd>
          </button>
          <button
            type="button"
            className="rail-row rail-row--nav"
            aria-pressed={view === "scheduled"}
            onClick={() => {
              setView((current) => (current === "scheduled" ? "work" : "scheduled"));
            }}
          >
            <Glyph icon="clock" hue="orange" size={28} />
            <span>Scheduled</span>
          </button>
          <button
            type="button"
            className="rail-row rail-row--nav"
            aria-pressed={view === "models"}
            onClick={() => {
              setView((current) => (current === "models" ? "work" : "models"));
            }}
          >
            <Glyph icon="agent" hue="purple" size={28} />
            <span>Models and credentials</span>
          </button>
          <button
            type="button"
            className="rail-row rail-row--nav"
            aria-pressed={view === "apps"}
            onClick={() => {
              setView((current) => (current === "apps" ? "work" : "apps"));
            }}
          >
            <Glyph icon="board" hue="orange" size={28} />
            <span>Apps</span>
          </button>

          <nav className="rail-scroll" aria-label="Recent work">
            {mode === "chat" ? (
              <section>
                <h2 className="rail-heading">Chats</h2>
                {chats.length === 0 ? (
                  <p className="rail-empty">No chats yet.</p>
                ) : (
                  <ul className="rail-list">
                    {chats.map((chat) => (
                      <li key={chat.conversationId}>
                        <button
                          type="button"
                          className="rail-row"
                          data-selected={selectedChatId === chat.conversationId}
                          aria-current={selectedChatId === chat.conversationId ? "true" : undefined}
                          onClick={() => {
                            setSelectedChatId(chat.conversationId);
                            setView("work");
                          }}
                        >
                          <span>{chat.title}</span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            ) : (
              <>
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
                            className="rail-row rail-row--project"
                            data-selected={inspection?.project.projectId === project.projectId}
                            aria-current={
                              inspection?.project.projectId === project.projectId
                                ? "true"
                                : undefined
                            }
                            onClick={() => void selectProject(project.projectId)}
                          >
                            <Glyph icon="doc" hue="blue" size={26} />
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
              </>
            )}
          </nav>

          <button
            type="button"
            className="rail-row rail-row--nav"
            aria-pressed={view === "settings"}
            onClick={() => {
              setView((current) => (current === "settings" ? "work" : "settings"));
            }}
          >
            <Glyph icon="gear" hue="blue" size={28} />
            <span>Settings</span>
          </button>
          <footer className="rail-status">
            <span className="status-dot" data-state={status.kind} aria-hidden="true" />
            <span>{describeCoreStatus(status)}</span>
          </footer>
        </aside>
      )}

      <main className="workspace">
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
          <SettingsView />
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
          />
        ) : view === "models" ? (
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
        ) : mode === "chat" ? (
          <>
            <header className="workspace-header">
              {!sidebarOpen && (
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Show sidebar"
                  onClick={() => {
                    setSidebarOpen(true);
                  }}
                >
                  <SidebarIcon />
                </button>
              )}
              <div className="workspace-title workspace-title--row">
                <h1>{selectedChat?.title ?? "New chat"}</h1>
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
              <div className="workspace-header-end">
                <span className="runtime-chip" data-mode={routingMode}>
                  {routingMode === "localOnly" && <LockIcon />}
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
                    <ShareIcon />
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
            <header className="workspace-header">
              {!sidebarOpen && (
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Show sidebar"
                  onClick={() => {
                    setSidebarOpen(true);
                  }}
                >
                  <SidebarIcon />
                </button>
              )}
              <div className="workspace-title">
                <h1>
                  {selectedConversation?.title ?? inspection?.project.name ?? "Open a project"}
                </h1>
                <p>
                  {inspection?.project.roots[0] ??
                    "Choose a local directory to create a canonical, restart-safe project record."}
                </p>
              </div>
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
              <ModeSwitch mode={mode} onChange={chooseMode} />
              <div className="workspace-header-end">
                {/* The mode the next task will start under, said in words; the
                  lock only repeats Local only. */}
                <span className="runtime-chip" data-mode={routingMode}>
                  {routingMode === "localOnly" && <LockIcon />}
                  {MODE_LABEL[routingMode]}
                </span>
                {selectedConversation !== undefined && (
                  <button
                    type="button"
                    className="pill-button"
                    onClick={() => {
                      setSharing(selectedConversation);
                    }}
                  >
                    <ShareIcon />
                    Share
                  </button>
                )}
                {/* Only while the panel is put away: once open, it closes from
                  its own button. */}
                {!detailsOpen && (
                  <button
                    type="button"
                    className="pill-button"
                    onClick={() => {
                      setDetailsOpen(true);
                    }}
                  >
                    Details
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
                    <Glyph icon="folder" hue="yellow" size={72} />
                    <h2 id="empty-heading">Start with an approved project root.</h2>
                    <p>
                      Tramontana stores project identity, conversations, task history, and
                      configuration manifests in the local profile. Markdown is parsed as
                      configuration data and never executed.
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
                        <p className="inline-empty">
                          No tasks yet. The first run will appear here.
                        </p>
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
                          <Glyph icon="agent" hue="purple" size={52} />
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
                          <span>
                            {stream.partial ? "live frame · joined part-way" : "live frame"}
                          </span>
                          <p>{stream.text}</p>
                        </div>
                      )}
                    </section>
                  </>
                )}
              </div>
            </div>

            {inspection !== undefined && !loadingProject && (
              <div className="composer-dock">
                <form
                  className="composer"
                  onSubmit={(submitted) => {
                    submitted.preventDefault();
                    onStart();
                  }}
                >
                  <label className="sr-only" htmlFor="goal">
                    Task goal
                  </label>
                  <PendingFiles files={agentFiles.files} onRemove={agentFiles.remove} />
                  <textarea
                    ref={goalInput}
                    id="goal"
                    value={goal}
                    placeholder="Describe a task for this project"
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
                    {offeredWorkers.length > 1 && (
                      <label className="composer-worker">
                        Worker
                        <select
                          value={chosenWorker?.agentId ?? ""}
                          onChange={(changed) => {
                            setWorker(changed.target.value);
                          }}
                        >
                          {offeredWorkers.map((choice) => (
                            <option key={choice.agentId} value={choice.agentId}>
                              {choice.displayName}
                              {choice.cloud === true ? " (off this device)" : ""}
                            </option>
                          ))}
                        </select>
                      </label>
                    )}
                    <label className="composer-toggle" data-checked={mayWrite}>
                      <input
                        type="checkbox"
                        checked={mayWrite}
                        onChange={(changed) => {
                          setMayWrite(changed.target.checked);
                        }}
                      />
                      May edit files
                    </label>
                    <label className="composer-toggle" data-checked={mayRunRecipes}>
                      <input
                        type="checkbox"
                        checked={mayRunRecipes}
                        onChange={(changed) => {
                          setMayRunRecipes(changed.target.checked);
                        }}
                      />
                      May run recipes
                    </label>
                    <label className="composer-toggle" data-checked={mayUseApps}>
                      <input
                        type="checkbox"
                        checked={mayUseApps}
                        onChange={(changed) => {
                          setMayUseApps(changed.target.checked);
                        }}
                      />
                      May use Apps
                    </label>
                    <label className="composer-toggle" data-checked={mayBrowse}>
                      <input
                        type="checkbox"
                        checked={mayBrowse}
                        onChange={(changed) => {
                          setMayBrowse(changed.target.checked);
                        }}
                      />
                      May browse the web
                    </label>
                    {offeredSkills.length > 0 && (
                      <fieldset className="composer-skills">
                        <legend>Skills this project offers</legend>
                        {listedSkills.map((skill) => (
                          <label key={skill.id} className="composer-skill">
                            <input
                              type="checkbox"
                              checked={chosenSkills.includes(skill.id)}
                              onChange={(changed) => {
                                setTicked((chosen) =>
                                  changed.target.checked
                                    ? [...chosen, skill.id]
                                    : chosen.filter((id) => id !== skill.id),
                                );
                              }}
                            />
                            <span>
                              {skill.displayName}
                              {suggested.includes(skill.id) && (
                                <em className="composer-suggested"> suggested for this goal</em>
                              )}
                              {skill.description === undefined ? "" : ` — ${skill.description}`}
                            </span>
                          </label>
                        ))}
                      </fieldset>
                    )}
                  </fieldset>
                  <div className="composer-footer">
                    <AttachButton onClick={agentFiles.choose} disabled={running} />
                    <span>
                      {configuration === undefined
                        ? "No configuration manifest"
                        : `${configuration.sources.filter((source) => source.active).length} active sources · ${configuration.manifestHash.slice(0, 12)}`}
                    </span>
                    <button
                      type="button"
                      className="button button--stop"
                      onClick={onCancel}
                      disabled={!running}
                    >
                      <span className="stop-square" aria-hidden="true" />
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
                <p className="composer-disclosure">
                  {describeChoices(
                    routingMode,
                    mayWrite,
                    mayRunRecipes,
                    mayUseApps,
                    mayBrowse,
                    chosenSkills.length,
                  )}
                </p>
              </div>
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
            <CloseIcon />
          </button>
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

function SidebarIcon(): React.JSX.Element {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <rect x="3" y="4" width="18" height="16" rx="3" />
      <path d="M9 4v16" />
    </svg>
  );
}

function CloseIcon(): React.JSX.Element {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      aria-hidden="true"
    >
      <path d="M6 6l12 12M18 6 6 18" />
    </svg>
  );
}

function ShareIcon(): React.JSX.Element {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M12 15V3M7 8l5-5 5 5" />
      <path d="M5 13v6a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-6" />
    </svg>
  );
}

function LockIcon(): React.JSX.Element {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <rect x="5" y="11" width="14" height="10" rx="2" />
      <path d="M8 11V7a4 4 0 0 1 8 0v4" />
    </svg>
  );
}
