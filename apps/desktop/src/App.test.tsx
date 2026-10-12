import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ApprovalRequest,
  ConversationRecord,
  EffectiveConfiguration,
  EventEnvelope,
  EventPayload,
  ProjectRecord,
  TaskRecord,
  TaskUpdate,
} from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const fetchCoreInfo = vi.hoisted(() => vi.fn());
const openProject = vi.hoisted(() => vi.fn());
const listProjects = vi.hoisted(() => vi.fn());
const inspectProject = vi.hoisted(() => vi.fn());
const createConversation = vi.hoisted(() => vi.fn());
const listConversations = vi.hoisted(() => vi.fn());
const listTasks = vi.hoisted(() => vi.fn());
const replayTaskEvents = vi.hoisted(() => vi.fn());
const startTask = vi.hoisted(() => vi.fn());
const cancelTask = vi.hoisted(() => vi.fn());
const listPendingApprovals = vi.hoisted(() => vi.fn());
const onApprovalRequested = vi.hoisted(() => vi.fn());
const resolveApproval = vi.hoisted(() => vi.fn());
const projectChanges = vi.hoisted(() => vi.fn());
const listConfiguredEndpoints = vi.hoisted(() => vi.fn());
const listModelEndpoints = vi.hoisted(() => vi.fn());
const listProjectGrants = vi.hoisted(() => vi.fn());
const watchTask = vi.hoisted(() => vi.fn());
const listApps = vi.hoisted(() => vi.fn());
const latestCheckpoint = vi.hoisted(() => vi.fn());
const listWorkers = vi.hoisted(() => vi.fn());
const listSkills = vi.hoisted(() => vi.fn());
const suggestSkills = vi.hoisted(() => vi.fn());
const deleteProject = vi.hoisted(() => vi.fn());
const listPersonalSkills = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return {
    ...actual,
    fetchCoreInfo,
    openProject,
    listProjects,
    inspectProject,
    createConversation,
    listConversations,
    listTasks,
    replayTaskEvents,
    startTask,
    cancelTask,
    listPendingApprovals,
    onApprovalRequested,
    resolveApproval,
    projectChanges,
    listConfiguredEndpoints,
    listModelEndpoints,
    listProjectGrants,
    watchTask,
    latestCheckpoint,
    listApps,
    listWorkers,
    listSkills,
    suggestSkills,
    deleteProject,
    listPersonalSkills,
  };
});

const { App } = await import("./App.js");

const project: ProjectRecord = {
  schemaVersion: 1,
  projectId: "project-1",
  name: "Tramontana",
  roots: ["/work/tramontana"],
  createdAt: "2026-01-01T00:00:00.000Z",
};

const configuration: EffectiveConfiguration = {
  schemaVersion: 1,
  sources: [
    {
      sourceId: "AGENTS.md",
      relativePath: "AGENTS.md",
      kind: "rootAgents",
      enabled: true,
      active: true,
      precedence: 5,
      contentHash: "a".repeat(64),
      byteCount: 120,
    },
  ],
  diagnostics: [],
  manifestHash: "b".repeat(64),
  resolvedAt: "2026-01-01T00:00:01.000Z",
};

const conversation: ConversationRecord = {
  schemaVersion: 1,
  conversationId: "conversation-1",
  projectId: project.projectId,
  title: "M2 Conversation",
  createdAt: "2026-01-01T00:00:02.000Z",
};

const task: TaskRecord = {
  schemaVersion: 1,
  taskId: "task-existing",
  conversationId: conversation.conversationId,
  projectId: project.projectId,
  goalSummary: "Existing durable task",
  configurationManifestHash: configuration.manifestHash,
  state: "completed",
  lastSequence: 12,
  createdAt: "2026-01-01T00:00:03.000Z",
  updatedAt: "2026-01-01T00:00:04.000Z",
};

function event(sequence: number, payload: EventPayload): EventEnvelope {
  return {
    schemaVersion: 1,
    eventId: `event-${sequence}`,
    taskId: "task-1",
    conversationId: conversation.conversationId,
    sequence,
    occurredAt: "2026-01-01T00:00:00.000Z",
    kind: payload.kind as EventEnvelope["kind"],
    visibility: "user",
    correlationId: "c1",
    payload,
  };
}

function approval(approvalId: string): ApprovalRequest {
  return {
    schemaVersion: 1,
    approvalId,
    taskId: "task-1",
    capability: "fs.write",
    toolId: "builtin.fs.patch",
    requestedScope: { projectId: project.projectId, roots: project.roots, releasesData: false },
    riskClass: "modify",
    sideEffect: "localReversible",
    summary: `builtin.fs.patch wants to write ${approvalId}`,
    details: [],
    eligibleDecisions: ["allowOnce", "deny"],
  };
}

function captureUpdates(): { emit: (update: TaskUpdate) => void } {
  let sink: (update: TaskUpdate) => void = () => undefined;
  startTask.mockImplementation(
    (_goal: string, onUpdate: (update: TaskUpdate) => void, _conversationId?: string) => {
      sink = onUpdate;
      return Promise.resolve("task-1");
    },
  );
  return {
    emit: (update) => {
      sink(update);
    },
  };
}

/**
 * Agent mode opens on its home screen; the tests below are about work in a
 * conversation, so they open the project's one from the sidebar first, and
 * write the goal most of them start.
 */
async function openConversation(): Promise<void> {
  fireEvent.click(await screen.findByRole("button", { name: "M2 Conversation" }));
  await screen.findByRole("heading", { name: "M2 Conversation" });
  const goal = screen.getByLabelText<HTMLTextAreaElement>("Task goal");
  if (!goal.disabled) fireEvent.change(goal, { target: { value: "Say hello" } });
}

async function renderLoaded(): Promise<void> {
  render(<App />);
  await openConversation();
}

/** Write the goal again: a started task clears the box, as Chat does. */
function writeGoal(value = "Say hello"): void {
  fireEvent.change(screen.getByLabelText("Task goal"), { target: { value } });
}

/** Open one of the composer's chips by what it chooses: "Access", "Skills". */
async function openChip(user: ReturnType<typeof userEvent.setup>, name: string): Promise<void> {
  await user.click(screen.getByRole("button", { name: new RegExp(`^${name}:`) }));
}

/** Open a part of Settings: "Models and credentials", "Customize". */
async function openSettings(user: ReturnType<typeof userEvent.setup>, part: string): Promise<void> {
  if (screen.queryByRole("heading", { name: "Settings", level: 1 }) === null) {
    await user.click(screen.getByRole("button", { name: /Settings/ }));
  }
  await user.click(screen.getByRole("tab", { name: part }));
}

/**
 * The name a screen reader announces for a control, the ways this window
 * gives one: `aria-labelledby`, `aria-label`, an associated `<label>`, or the
 * control's own text.
 */
function accessibleName(element: HTMLElement): string {
  const labelledBy = element.getAttribute("aria-labelledby");
  if (labelledBy !== null) {
    return labelledBy
      .split(/\s+/)
      .map((id) => document.getElementById(id)?.textContent ?? "")
      .join(" ")
      .trim();
  }
  const label = element.getAttribute("aria-label");
  if (label !== null) return label.trim();
  if (
    element instanceof HTMLInputElement ||
    element instanceof HTMLTextAreaElement ||
    element instanceof HTMLSelectElement
  ) {
    const labels = [...(element.labels ?? [])].map((each) => each.textContent);
    if (labels.join("").trim() !== "") return labels.join(" ").trim();
  }
  return element.textContent.trim();
}

/** Every control on screen that a screen reader could not name. */
function unnamedControls(): string[] {
  const roles = [
    "button",
    "checkbox",
    "radio",
    "textbox",
    "combobox",
    "link",
    "tab",
    "switch",
  ] as const;
  return roles.flatMap((role) =>
    screen
      .queryAllByRole(role)
      .filter((element) => accessibleName(element) === "")
      .map((element) => `${role}: ${element.outerHTML.slice(0, 120)}`),
  );
}

/** Ids used more than once, which break every label that points at one. */
function duplicateIds(): string[] {
  const seen = new Map<string, number>();
  for (const element of document.querySelectorAll("[id]")) {
    seen.set(element.id, (seen.get(element.id) ?? 0) + 1);
  }
  return [...seen].filter(([, count]) => count > 1).map(([id]) => id);
}

describe("M2 durable desktop surface", () => {
  beforeEach(() => {
    // These tests are about work in a project, which is Agent mode; the
    // window otherwise opens in Chat (ADR 0057).
    localStorage.setItem("tramontana.mode", "agent");
    for (const mock of [
      fetchCoreInfo,
      openProject,
      listProjects,
      inspectProject,
      createConversation,
      listConversations,
      listTasks,
      replayTaskEvents,
      startTask,
      cancelTask,
      listPendingApprovals,
      onApprovalRequested,
      resolveApproval,
      projectChanges,
      listConfiguredEndpoints,
      listModelEndpoints,
      listProjectGrants,
      watchTask,
      latestCheckpoint,
      listApps,
      deleteProject,
      listPersonalSkills,
    ]) {
      mock.mockReset();
    }
    watchTask.mockResolvedValue(() => undefined);
    latestCheckpoint.mockResolvedValue(undefined);
    listApps.mockResolvedValue([]);
    listWorkers.mockResolvedValue([]);
    listSkills.mockResolvedValue([]);
    listPersonalSkills.mockResolvedValue([]);
    suggestSkills.mockResolvedValue([]);
    listProjectGrants.mockResolvedValue([]);
    listConfiguredEndpoints.mockResolvedValue([]);
    listModelEndpoints.mockResolvedValue([]);
    projectChanges.mockResolvedValue({ schemaVersion: 1, status: "", diff: "", truncated: false });
    listPendingApprovals.mockResolvedValue([]);
    onApprovalRequested.mockResolvedValue(() => undefined);
    resolveApproval.mockResolvedValue(undefined);
    fetchCoreInfo.mockResolvedValue({ schemaVersion: 1, appVersion: "0.1.0" });
    listProjects.mockResolvedValue([project]);
    inspectProject.mockResolvedValue({ schemaVersion: 1, project, configuration });
    listConversations.mockResolvedValue([conversation]);
    listTasks.mockResolvedValue([task]);
    replayTaskEvents.mockResolvedValue([]);
    openProject.mockResolvedValue({ schemaVersion: 1, project, configuration });
    createConversation.mockResolvedValue(conversation);
    startTask.mockResolvedValue("task-1");
    cancelTask.mockResolvedValue(undefined);
  });

  it("loads recent projects, conversations, tasks, and the effective manifest", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    expect(screen.getByText("Existing durable task")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByText("/work/tramontana")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Project inspector" })).toHaveTextContent(
      configuration.manifestHash,
    );
    expect(screen.getByText("AGENTS.md")).toBeInTheDocument();
    expect(replayTaskEvents).toHaveBeenCalledWith(task.taskId);
  });

  it("opens in Chat, lists the chats, and switches to Agent and back", async () => {
    const user = userEvent.setup();
    localStorage.removeItem("tramontana.mode");
    const chat: ConversationRecord = {
      schemaVersion: 1,
      conversationId: "chat-1",
      title: "Winds",
      createdAt: "2026-01-01T00:00:09.000Z",
    };
    listConversations.mockImplementation((projectId?: string) =>
      Promise.resolve(projectId === undefined ? [chat] : [conversation]),
    );
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "What can I help with?" }),
    ).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Winds" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
    // A chat has no project, so there is no project inspector to show.
    expect(
      screen.queryByRole("complementary", { name: "Project inspector" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("radio", { name: "Agent" }));
    // Agent opens on its home: a greeting, the project ready for a task, and
    // no conversation picked for the person.
    expect(await screen.findByRole("heading", { name: "What's up next?" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Project: Tramontana" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Agent" })).toHaveAttribute("aria-checked", "true");
    expect(localStorage.getItem("tramontana.mode")).toBe("agent");

    await user.click(screen.getByRole("radio", { name: "Chat" }));
    expect(screen.getByRole("heading", { name: "New chat", level: 1 })).toBeInTheDocument();
  });

  it("brings the sidebar back from a screen with no work header", async () => {
    // Regression: collapsing the sidebar on Settings, Scheduled, Projects,
    // Models or Apps left no button to show it again.
    const user = userEvent.setup();
    await renderLoaded();
    await openSettings(user, "Models and credentials");
    await user.click(screen.getByRole("button", { name: "Hide sidebar" }));
    expect(
      screen.queryByRole("complementary", { name: "Projects and conversations" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Show sidebar" }));
    expect(
      screen.getByRole("complementary", { name: "Projects and conversations" }),
    ).toBeInTheDocument();
  });

  it("puts the sidebar and the details away, and brings them back", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Hide sidebar" }));
    expect(
      screen.queryByRole("complementary", { name: "Projects and conversations" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Show sidebar" }));
    expect(
      screen.getByRole("complementary", { name: "Projects and conversations" }),
    ).toBeInTheDocument();

    // The details start put away; their button brings them out and hides
    // itself, and the panel closes from its own button.
    expect(
      screen.queryByRole("complementary", { name: "Project inspector" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByRole("complementary", { name: "Project inspector" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Details" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Close details" }));
    expect(
      screen.queryByRole("complementary", { name: "Project inspector" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Details" })).toBeInTheDocument();
  });

  it("starts from the home screen in a conversation of its own, named after the goal", async () => {
    const user = userEvent.setup();
    const created: ConversationRecord = {
      ...conversation,
      conversationId: "conversation-new",
      title: "Check the menu prices for mistakes",
    };
    createConversation.mockResolvedValue(created);
    render(<App />);

    // Nothing is opened for the person: the project waits on the home screen.
    await screen.findByRole("button", { name: "Project: Tramontana" });
    expect(screen.getByRole("heading", { name: "New task", level: 1 })).toBeInTheDocument();
    expect(listTasks).not.toHaveBeenCalledWith(conversation.conversationId);
    expect(replayTaskEvents).not.toHaveBeenCalled();

    await user.type(
      screen.getByLabelText("Task goal"),
      "Check the menu prices for mistakes{Shift>}{Enter}{/Shift}and list each one",
    );
    // Shift+Return made a new line; nothing started.
    expect(createConversation).not.toHaveBeenCalled();
    await user.keyboard("{Enter}");

    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Check the menu prices for mistakes\nand list each one",
        expect.any(Function),
        "conversation-new",
        expect.objectContaining({ routingMode: "automatic" }),
      );
    });
    // Named from the goal's first line, before the task started in it.
    expect(createConversation).toHaveBeenCalledWith(
      project.projectId,
      "Check the menu prices for mistakes",
    );
    expect(createConversation.mock.invocationCallOrder[0]).toBeLessThan(
      startTask.mock.invocationCallOrder[0] ?? 0,
    );
    expect(
      await screen.findByRole("heading", { name: "Check the menu prices for mistakes" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Task goal")).toHaveValue("");
  });

  it("lists each project's conversations under it, and opens one from another project", async () => {
    const other: ProjectRecord = {
      ...project,
      projectId: "project-2",
      name: "Harbor Cafe",
      roots: ["/work/harbor"],
    };
    const elsewhere: ConversationRecord = {
      ...conversation,
      conversationId: "conversation-2",
      projectId: other.projectId,
      title: "Menu prices",
    };
    listProjects.mockResolvedValue([project, other]);
    listConversations.mockImplementation((projectId?: string) =>
      Promise.resolve(projectId === other.projectId ? [elsewhere] : [conversation]),
    );
    inspectProject.mockImplementation((projectId: string) =>
      Promise.resolve({
        schemaVersion: 1,
        project: projectId === other.projectId ? other : project,
        configuration,
      }),
    );
    const user = userEvent.setup();
    render(<App />);

    // The open project is shown open; the others are closed until asked.
    const open = await screen.findByRole("button", { name: "Tramontana" });
    expect(open).toHaveAttribute("aria-expanded", "true");
    expect(await screen.findByRole("button", { name: "M2 Conversation" })).toBeInTheDocument();
    const closed = screen.getByRole("button", { name: "Harbor Cafe" });
    expect(closed).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("button", { name: "Menu prices" })).not.toBeInTheDocument();

    await user.click(closed);
    await user.click(await screen.findByRole("button", { name: "Menu prices" }));
    expect(await screen.findByRole("heading", { name: "Menu prices" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Project: Harbor Cafe" })).toBeInTheDocument();

    // A project's own "+" goes to its home, ready for a new task.
    await user.click(screen.getByRole("button", { name: "New task in Tramontana" }));
    expect(await screen.findByRole("button", { name: "Project: Tramontana" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "New task", level: 1 })).toBeInTheDocument();
  });

  it("keeps projects in the sidebar and models and connectors in Settings, in both modes", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("button", { name: "Project: Tramontana" });

    for (const mode of ["Agent", "Chat"]) {
      await user.click(screen.getByRole("radio", { name: mode }));
      const sidebar = within(
        screen.getByRole("complementary", { name: "Projects and conversations" }),
      );
      expect(sidebar.getByRole("button", { name: "Scheduled" })).toBeInTheDocument();
      expect(sidebar.getByRole("button", { name: "Projects" })).toBeInTheDocument();
      expect(sidebar.getByRole("button", { name: "Open project" })).toBeInTheDocument();
      expect(sidebar.queryByRole("button", { name: "More" })).not.toBeInTheDocument();
      expect(
        sidebar.queryByRole("button", { name: "Models and credentials" }),
      ).not.toBeInTheDocument();
    }

    await openSettings(user, "Customize");
    expect(await screen.findByRole("heading", { name: "Apps", level: 2 })).toBeInTheDocument();
    for (const part of ["General", "Customize", "Models and credentials", "Data"]) {
      expect(screen.getByRole("tab", { name: part })).toBeInTheDocument();
    }
  });

  it("removes a project from its right-click menu, after asking, and opens the next one", async () => {
    const other: ProjectRecord = {
      ...project,
      projectId: "project-2",
      name: "Harbor Cafe",
      roots: ["/work/harbor"],
    };
    let listed = [project, other];
    listProjects.mockImplementation(() => Promise.resolve(listed));
    deleteProject.mockImplementation(() => {
      listed = [other];
      return Promise.resolve(undefined);
    });
    inspectProject.mockImplementation((projectId: string) =>
      Promise.resolve({
        schemaVersion: 1,
        project: projectId === other.projectId ? other : project,
        configuration,
      }),
    );
    const user = userEvent.setup();
    render(<App />);
    const row = await screen.findByRole("button", { name: "Tramontana" });

    fireEvent.contextMenu(row, { clientX: 40, clientY: 200 });
    const menu = screen.getByRole("menu", { name: "Tramontana options" });
    await user.click(within(menu).getByRole("menuitem", { name: /Remove from Tramontana/ }));

    const dialog = screen.getByRole("alertdialog", { name: "Remove Tramontana from Tramontana?" });
    expect(dialog).toHaveTextContent("The folder and its files are not touched");
    expect(dialog).toHaveTextContent("/work/tramontana");
    // Keep is the safe answer, and changes nothing.
    await user.click(within(dialog).getByRole("button", { name: "Keep" }));
    expect(deleteProject).not.toHaveBeenCalled();

    fireEvent.contextMenu(row, { clientX: 40, clientY: 200 });
    await user.click(screen.getByRole("menuitem", { name: /Remove from Tramontana/ }));
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => {
      expect(deleteProject).toHaveBeenCalledWith(project.projectId);
    });
    // The open project went; the next one opens in its place.
    expect(await screen.findByRole("button", { name: "Project: Harbor Cafe" })).toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("opens a native-picked project and refreshes the durable recents", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Open project" }));
    await waitFor(() => {
      expect(openProject).toHaveBeenCalledOnce();
    });
    expect(listProjects).toHaveBeenCalledTimes(2);
    expect(inspectProject).toHaveBeenCalledWith(project.projectId);
  });

  it("reports a compatible core once the handshake resolves", async () => {
    await renderLoaded();
    expect(screen.getByText(/core 0\.1\.0 · contracts v1/i)).toBeInTheDocument();
  });

  it("reports a core whose contract version this build does not speak", async () => {
    fetchCoreInfo.mockResolvedValue({ schemaVersion: 99, appVersion: "0.1.0" });
    await renderLoaded();
    expect(screen.getByText(/speaks contracts v99/i)).toBeInTheDocument();
  });

  it("stays inspectable when the core handshake is unreachable", async () => {
    fetchCoreInfo.mockRejectedValue(new Error("no ipc"));
    await renderLoaded();
    expect(screen.getByText(/core unavailable/i)).toBeInTheDocument();
  });

  it("renders a newly started task stream in order", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      // Read-only and Automatic unless the person chose otherwise.
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        { writes: false, recipes: false, apps: false, browse: false, routingMode: "automatic" },
      );
    });
    updates.emit({
      kind: "event",
      event: event(1, { kind: "taskCreated", goalSummary: "Say hello" }),
    });
    updates.emit({
      kind: "event",
      event: event(2, {
        kind: "taskStateChanged",
        previous: "queued",
        current: "running",
        reasonCode: "worker.started",
      }),
    });

    // The full record sits behind a disclosure below the cards.
    await user.click(await screen.findByText("All events (2)"));
    const list = await screen.findByRole("list", { name: "Task event stream" });
    const rendered = within(list).getAllByRole("listitem");
    expect(rendered).toHaveLength(2);
    expect(rendered[0]).toHaveTextContent("goal: Say hello");
    expect(rendered[1]).toHaveTextContent("queued → running");
  });

  it("ignores duplicate durable sequences and keeps frames out of the event list", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    await renderLoaded();
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });

    const created = event(1, { kind: "taskCreated", goalSummary: "Say hello" });
    updates.emit({ kind: "event", event: created });
    updates.emit({ kind: "event", event: created });
    updates.emit({ kind: "frame", frame: { streamId: "s", offset: 0, text: "Hel" } });
    updates.emit({ kind: "frame", frame: { streamId: "s", offset: 3, text: "lo" } });

    await user.click(await screen.findByText("All events (1)"));
    const list = await screen.findByRole("list", { name: "Task event stream" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByText("Hello")).toBeInTheDocument();
  });

  it("enables Stop only while a selected task is running", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    await renderLoaded();

    // Nothing to stop yet: the composer offers to start.
    expect(screen.queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });
    updates.emit({
      kind: "event",
      event: event(1, { kind: "taskCreated", goalSummary: "Say hello" }),
    });
    const stop = await screen.findByRole("button", { name: "Stop" });

    await user.click(stop);
    expect(cancelTask).toHaveBeenCalledWith("task-1");

    updates.emit({
      kind: "event",
      event: event(2, {
        kind: "taskStateChanged",
        previous: "queued",
        current: "cancelled",
        reasonCode: "cleanup.complete",
      }),
    });
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
    });
  });

  it("opens a terminal when the task runs a command, showing what it printed", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    await renderLoaded();
    expect(screen.queryByRole("region", { name: "Terminal" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Terminal" })).toBeNull();

    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });
    updates.emit({
      kind: "event",
      event: event(1, { kind: "taskCreated", goalSummary: "Say hello" }),
    });
    updates.emit({
      kind: "event",
      event: event(2, {
        kind: "toolRequested",
        invocationId: "run-1",
        toolId: "builtin.recipe.run",
        argumentsSummary: "recipe test",
        riskClass: "dangerous",
      }),
    });

    // It opens by itself the moment a command is asked for.
    const terminal = await screen.findByRole("region", { name: "Terminal" });
    expect(terminal).toHaveTextContent("$ recipe test");
    expect(terminal).toHaveTextContent("waiting for your approval");

    updates.emit({
      kind: "event",
      event: event(3, {
        kind: "messageCommitted",
        message: {
          schemaVersion: 1,
          messageId: "m-1",
          role: "tool",
          createdAt: "2026-01-01T00:00:01.000Z",
          parts: [
            {
              kind: "toolResult",
              invocationId: "run-1",
              content: { exitCode: 0, stdout: "Ran 2 tests\nOK", stderr: "" },
              truncated: false,
            },
          ],
        },
      } as unknown as EventPayload),
    });
    updates.emit({
      kind: "event",
      event: event(4, {
        kind: "toolCompleted",
        invocationId: "run-1",
        displaySummary: "Recipe `test` passed",
        sideEffect: "external",
        truncated: false,
      }),
    });
    await waitFor(() => {
      expect(terminal).toHaveTextContent("done · exit 0");
    });
    expect(terminal).toHaveTextContent("Ran 2 tests");

    // Closed, it waits behind a header button.
    await user.click(within(terminal).getByRole("button", { name: "Close terminal" }));
    expect(screen.queryByRole("region", { name: "Terminal" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Terminal" }));
    expect(screen.getByRole("region", { name: "Terminal" })).toBeInTheDocument();
  });

  it("puts a question raised before this window opened to the person", async () => {
    listPendingApprovals.mockResolvedValue([approval("approval-1")]);
    const user = userEvent.setup();
    await renderLoaded();

    expect(await screen.findByRole("heading", { name: "Approval needed" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Allow once" }));
    await waitFor(() => {
      expect(resolveApproval).toHaveBeenCalledWith("approval-1", "allowOnce");
    });
    await waitFor(() => {
      expect(screen.queryByRole("heading", { name: "Approval needed" })).not.toBeInTheDocument();
    });
  });

  it("queues a second question behind the one on screen", async () => {
    listPendingApprovals.mockResolvedValue([approval("approval-1")]);
    let raise: (request: ApprovalRequest) => void = () => undefined;
    onApprovalRequested.mockImplementation((handler: (request: ApprovalRequest) => void) => {
      raise = handler;
      return Promise.resolve(() => undefined);
    });
    await renderLoaded();
    await screen.findByRole("heading", { name: "Approval needed" });

    act(() => {
      raise(approval("approval-2"));
    });

    expect(
      await screen.findByText("1 more question is waiting behind this one."),
    ).toBeInTheDocument();
    // The first question is still the one being asked.
    expect(screen.getByText("builtin.fs.patch wants to write approval-1")).toBeInTheDocument();
  });

  it("starts a task with the mode and access the person chose, and says what they mean", async () => {
    const user = userEvent.setup();
    await renderLoaded();
    expect(screen.getByText(/Read-only: the model can look/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Access: Read-only" })).toBeInTheDocument();

    await openChip(user, "Where models run");
    await user.click(screen.getByRole("radio", { name: "Local only" }));
    expect(
      screen.getByRole("button", { name: "Where models run: Local only" }),
    ).toBeInTheDocument();
    await openChip(user, "Access");
    await user.click(screen.getByRole("checkbox", { name: "May edit files" }));
    await user.click(screen.getByRole("checkbox", { name: "May run recipes" }));
    expect(
      screen.getByText(/Local only governs where models run, not the commands a recipe runs/),
    ).toBeInTheDocument();
    expect(screen.getByText(/each one is put to you before it runs/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        { writes: true, recipes: true, apps: false, browse: false, routingMode: "localOnly" },
      );
    });
  });

  it("offers a task the Apps only when the person allows them, and says so", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await openChip(user, "Access");
    await user.click(screen.getByRole("checkbox", { name: "May use Apps" }));
    expect(screen.getByText(/may ask to use the Apps' reading tools/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        { writes: false, recipes: false, apps: true, browse: false, routingMode: "automatic" },
      );
    });
  });

  it("runs a task on the worker the person picked, and offers no cloud one under Local only", async () => {
    listWorkers.mockResolvedValue([
      { agentId: "builtin.reference", displayName: "Automatic" },
      { agentId: "builtin.reference@local/qwen", displayName: "Qwen", cloud: false },
      { agentId: "builtin.reference@cloud/claude", displayName: "Claude", cloud: true },
    ]);
    const user = userEvent.setup();
    await renderLoaded();

    await screen.findByRole("button", { name: /^Worker:/ });
    await openChip(user, "Worker");
    const choice = screen.getByRole("combobox", { name: "Worker" });
    await user.selectOptions(choice, "builtin.reference@cloud/claude");
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        expect.objectContaining({ worker: "builtin.reference@cloud/claude" }),
      );
    });

    // Local only cannot reach it, so it is not offered, and the task falls
    // back to a worker that can run.
    writeGoal();
    await openChip(user, "Where models run");
    await user.click(screen.getByRole("radio", { name: "Local only" }));
    await openChip(user, "Worker");
    expect(
      [...screen.getByRole<HTMLSelectElement>("combobox", { name: "Worker" }).options].map(
        (option) => option.text,
      ),
    ).toEqual(["Automatic", "Qwen"]);
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenLastCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        expect.objectContaining({ worker: "builtin.reference", routingMode: "localOnly" }),
      );
    });
  });

  it("gives a task the project's skills chosen with /, and none otherwise", async () => {
    listSkills.mockResolvedValue([
      { id: "review", displayName: "Review Rust", description: "Look for unsafe code." },
      { id: "release", displayName: "Release checklist" },
    ]);
    const user = userEvent.setup();
    await renderLoaded();

    // Offering is not selecting: nothing is chosen until the person picks it.
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        expect.not.objectContaining({ skills: expect.anything() }),
      );
    });

    const goal = screen.getByLabelText("Task goal");
    await user.type(goal, "Check it /rev");
    const menu = await screen.findByRole("listbox", { name: "Skills" });
    expect(within(menu).getAllByRole("option")).toHaveLength(1);
    expect(within(menu).getByRole("option")).toHaveTextContent("/review");
    await user.keyboard("{Enter}");
    // The `/word` was only how it was asked for; the skill is now a chip.
    expect(goal).toHaveValue("Check it ");
    expect(screen.getByRole("list", { name: "Skills for this message" })).toHaveTextContent(
      "/review",
    );
    expect(screen.getByText(/It is given one skill to follow\./)).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenLastCalledWith(
        "Check it",
        expect.any(Function),
        conversation.conversationId,
        expect.objectContaining({ skills: ["review"] }),
      );
    });
    // It went with that message; the next starts with none.
    await waitFor(() => {
      expect(screen.queryByRole("list", { name: "Skills for this message" })).toBeNull();
    });
  });

  it("offers the person's own skills with /, and gives them to the task", async () => {
    listPersonalSkills.mockResolvedValue([
      {
        schemaVersion: 1,
        id: "plain-english",
        name: "Plain English",
        description: "Short sentences.",
        contentHash: "h",
        byteCount: 40,
      },
    ]);
    const user = userEvent.setup();
    await renderLoaded();

    const goal = screen.getByLabelText("Task goal");
    await user.clear(goal);
    await user.type(goal, "/pla");
    const option = await screen.findByRole("option", { name: /plain-english/ });
    expect(option).toHaveTextContent("your skill");
    await user.click(option);
    await user.type(goal, "Explain the menu");
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Explain the menu",
        expect.any(Function),
        conversation.conversationId,
        expect.objectContaining({ personalSkills: ["plain-english"] }),
      );
    });
  });

  it("marks the skills that match the goal and lists them first, choosing none", async () => {
    listSkills.mockResolvedValue([
      { id: "release", displayName: "Release checklist" },
      { id: "rust-quality", displayName: "Rust quality pass", description: "Format and lint." },
    ]);
    suggestSkills.mockResolvedValue(["rust-quality"]);
    const user = userEvent.setup();
    await renderLoaded();

    const goal = screen.getByLabelText("Task goal");
    await user.type(goal, " /");
    const menu = await screen.findByRole("listbox", { name: "Skills" });
    await waitFor(() => {
      expect(within(menu).getAllByRole("option")[0]).toHaveTextContent("suggested for this goal");
    });
    const options = within(menu).getAllByRole("option");
    expect(options[0]).toHaveTextContent("/rust-quality");
    expect(options[1]).toHaveTextContent("/release");
    // Escape puts the menu away without choosing.
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("listbox", { name: "Skills" })).toBeNull();

    // Suggesting is not selecting: the task carries nothing until one is picked.
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello /",
        expect.any(Function),
        conversation.conversationId,
        expect.not.objectContaining({ skills: expect.anything() }),
      );
    });
  });

  it("shows no skills chooser for a project that offers none", async () => {
    await renderLoaded();
    expect(screen.queryByRole("group", { name: "Skills this project offers" })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Skills:/ })).toBeNull();
  });

  it("lets a task browse only when the person allows it, and says what Local only does not cover", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await openChip(user, "Where models run");
    await user.click(screen.getByRole("radio", { name: "Local only" }));
    await openChip(user, "Access");
    await user.click(screen.getByRole("checkbox", { name: "May browse the web" }));
    expect(screen.getByText(/may ask to open websites in a window of its own/)).toBeInTheDocument();
    expect(
      screen.getByText(/Local only governs where models run, not the pages the browser opens/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        { writes: false, recipes: false, apps: false, browse: true, routingMode: "localOnly" },
      );
    });
  });

  it("shows who ran a task and where, each step as a card, and retries by asking again", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    await renderLoaded();
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });

    updates.emit({
      kind: "event",
      event: event(1, {
        kind: "taskCreated",
        goalSummary: "Patch the greeting",
        routingMode: "localOnly",
      }),
    });
    updates.emit({
      kind: "event",
      event: event(2, {
        kind: "toolRequested",
        invocationId: "i1",
        toolId: "builtin.fs.read",
        argumentsSummary: "1 argument(s): path",
        riskClass: "read",
      }),
    });
    updates.emit({
      kind: "event",
      event: event(3, {
        kind: "toolCompleted",
        invocationId: "i1",
        displaySummary: "read greeting.txt",
        sideEffect: "none",
        truncated: false,
      }),
    });
    updates.emit({
      kind: "event",
      event: event(4, {
        kind: "taskStateChanged",
        previous: "running",
        current: "failed",
        reasonCode: "worker.failed",
      }),
    });

    const summary = await screen.findByRole("region", { name: "Task summary" });
    expect(summary).toHaveTextContent("Local only");
    expect(summary).toHaveTextContent("Nothing released off this device");
    const cards = within(screen.getByRole("list", { name: "Task activity" })).getAllByRole(
      "listitem",
    );
    expect(cards).toHaveLength(1);
    expect(cards[0]).toHaveTextContent("Done");
    expect(cards[0]).toHaveTextContent("read greeting.txt");

    // Retry puts the goal back for the person to start again; it runs nothing.
    await user.click(within(summary).getByRole("button", { name: "Retry…" }));
    const goal = screen.getByLabelText("Task goal");
    expect(goal).toHaveValue("Patch the greeting");
    expect(goal).toHaveFocus();
    expect(startTask).toHaveBeenCalledTimes(1);
  });

  it("reads the working tree again when a task ends", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    await renderLoaded();
    await waitFor(() => {
      expect(projectChanges).toHaveBeenCalledTimes(1);
    });

    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });
    updates.emit({ kind: "event", event: event(1, { kind: "taskCompleted", summary: "Done" }) });

    await waitFor(() => {
      expect(projectChanges).toHaveBeenCalledTimes(2);
    });
  });

  it("offers to import a bundle into a profile with no project yet", async () => {
    listProjects.mockResolvedValue([]);
    const user = userEvent.setup();
    render(<App />);

    await user.click(await screen.findByRole("button", { name: "Details" }));
    expect(await screen.findByRole("button", { name: "Import a bundle…" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Export project…" })).not.toBeInTheDocument();
  });

  it("says what is selected and what state a task is in, not by color alone", async () => {
    replayTaskEvents.mockResolvedValue([
      event(1, { kind: "taskCreated", goalSummary: "Existing durable task" }),
    ]);
    const { container } = render(<App />);
    await openConversation();

    expect(screen.getByRole("button", { name: "Tramontana" })).toHaveAttribute(
      "aria-current",
      "true",
    );
    expect(screen.getByRole("button", { name: "M2 Conversation" })).toHaveAttribute(
      "aria-current",
      "true",
    );
    // A task tab is a button in a list, so it is announced as one.
    const tabs = screen.getByRole("list", { name: "Tasks in this conversation" });
    expect(within(tabs).getByRole("button", { name: /Existing durable task/ })).toHaveAttribute(
      "aria-current",
      "true",
    );

    // Colored dots carry nothing a screen reader needs; the words beside them do.
    for (const dot of container.querySelectorAll(".status-dot")) {
      expect(dot).toHaveAttribute("aria-hidden", "true");
    }
    const summary = await screen.findByRole("region", { name: "Task summary" });
    expect(summary.querySelector('[aria-live="polite"]')).toHaveTextContent("queued");
  });

  it("stops following a running task when the window goes", async () => {
    // The shell kept forwarding a task nobody was watching any more, until
    // the task ended.
    listTasks.mockResolvedValue([{ ...task, state: "running", lastSequence: 2 }]);
    replayTaskEvents.mockResolvedValue([
      event(1, { kind: "taskCreated", goalSummary: "Existing durable task" }),
      event(2, {
        kind: "taskStateChanged",
        previous: "queued",
        current: "running",
        reasonCode: "worker.started",
      }),
    ]);
    const stop = vi.fn();
    watchTask.mockResolvedValue(stop);

    const shown = render(<App />);
    await openConversation();
    await waitFor(() => {
      expect(watchTask).toHaveBeenCalledWith(task.taskId, 2, expect.any(Function));
    });
    expect(stop).not.toHaveBeenCalled();
    shown.unmount();
    expect(stop).toHaveBeenCalledOnce();
  });

  it("follows a task still running after a reload, from the last event it has", async () => {
    listTasks.mockResolvedValue([{ ...task, state: "running", lastSequence: 2 }]);
    replayTaskEvents.mockResolvedValue([
      event(1, { kind: "taskCreated", goalSummary: "Existing durable task" }),
      event(2, {
        kind: "taskStateChanged",
        previous: "queued",
        current: "running",
        reasonCode: "worker.started",
      }),
    ]);
    let deliver: (update: TaskUpdate) => void = () => undefined;
    watchTask.mockImplementation(
      (_taskId: string, _after: number, onUpdate: (update: TaskUpdate) => void) => {
        deliver = onUpdate;
        return Promise.resolve(() => undefined);
      },
    );
    const user = userEvent.setup();
    await renderLoaded();

    await waitFor(() => {
      expect(watchTask).toHaveBeenCalledWith(task.taskId, 2, expect.any(Function));
    });
    act(() => {
      deliver({
        kind: "event",
        event: event(3, {
          kind: "taskStateChanged",
          previous: "running",
          current: "completed",
          reasonCode: "worker.completed",
        }),
      });
    });

    await user.click(await screen.findByText("All events (3)"));
    const list = screen.getByRole("list", { name: "Task event stream" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(3);
  });

  it("neither loses nor repeats the answer when the window reloads mid-stream", async () => {
    // The task goes on in the shell while the window is gone, so a reloaded
    // window rejoins an answer already under way. Frames are transport, not
    // record (`CORE_CONTRACTS.md` §5): the ones it missed are not replayed,
    // and the whole answer arrives durably when the turn ends.
    const running = { ...task, state: "running" as const, lastSequence: 2 };
    listTasks.mockResolvedValue([running]);
    replayTaskEvents.mockResolvedValue([
      event(1, { kind: "taskCreated", goalSummary: "Existing durable task" }),
      event(2, { kind: "modelStarted", streamId: "stream-1", modelId: "qwen2.5-7b-instruct" }),
    ]);
    let deliver: (update: TaskUpdate) => void = () => undefined;
    watchTask.mockImplementation(
      (_taskId: string, _after: number, onUpdate: (update: TaskUpdate) => void) => {
        deliver = onUpdate;
        return Promise.resolve(() => undefined);
      },
    );

    const first = render(<App />);
    await openConversation();
    await waitFor(() => {
      expect(watchTask).toHaveBeenCalledWith(task.taskId, 2, expect.any(Function));
    });
    act(() => {
      deliver({ kind: "frame", frame: { streamId: "stream-1", offset: 0, text: "Hello, " } });
      deliver({ kind: "frame", frame: { streamId: "stream-1", offset: 7, text: "wor" } });
    });
    const answer = await screen.findByRole("region", { name: "Live answer" });
    expect(answer).toHaveTextContent("Hello, wor");
    expect(answer).not.toHaveTextContent("joined part-way");

    // The reload. The shell keeps answering; this window starts over from the
    // record, which does not yet hold the message being produced.
    first.unmount();
    render(<App />);
    await openConversation();
    await waitFor(() => {
      expect(watchTask).toHaveBeenCalledTimes(2);
    });
    act(() => {
      deliver({ kind: "frame", frame: { streamId: "stream-1", offset: 10, text: "ld." } });
    });

    // What it holds is the end of an answer, and it says so instead of
    // letting three characters read as everything the model said.
    const rejoined = await screen.findByRole("region", { name: "Live answer" });
    expect(rejoined).toHaveTextContent("ld.");
    expect(rejoined).toHaveTextContent("joined part-way");

    // The turn ends. The whole answer is in the record, once, and the frames
    // it was streamed in are gone rather than sitting above it.
    act(() => {
      deliver({
        kind: "event",
        event: event(3, {
          kind: "messageCommitted",
          message: {
            schemaVersion: 1,
            messageId: "message-1",
            role: "assistant",
            status: "committed",
            createdAt: "2026-01-01T00:00:05.000Z",
            parts: [{ kind: "text", text: "Hello, world." }],
          },
        }),
      });
    });
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Live answer" })).not.toBeInTheDocument();
    });
    expect(screen.getAllByText("Hello, world.")).toHaveLength(1);

    // And the record itself reads once through, with nothing repeated.
    const user = userEvent.setup();
    await user.click(await screen.findByText("All events (3)"));
    const list = screen.getByRole("list", { name: "Task event stream" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(3);
  });

  it("does not follow a task that has already ended", async () => {
    replayTaskEvents.mockResolvedValue([
      event(1, { kind: "taskCreated", goalSummary: "Existing durable task" }),
      event(2, {
        kind: "taskStateChanged",
        previous: "running",
        current: "completed",
        reasonCode: "worker.completed",
      }),
    ]);
    await renderLoaded();
    await screen.findByText("All events (2)");
    expect(watchTask).not.toHaveBeenCalled();
  });

  it("offers a new profile somewhere to start rather than an empty window", async () => {
    // The first thing a person who has never run Tramontana sees. Nothing is
    // selected, nothing can be, and every panel that needs a project is gone;
    // what is left has to say what to do.
    listProjects.mockResolvedValue([]);
    listConversations.mockResolvedValue([]);
    listTasks.mockResolvedValue([]);

    render(<App />);

    expect(await screen.findByText(/No projects yet/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open project" })).toBeEnabled();
    expect(screen.getByRole("heading", { name: "What's up next?" })).toBeInTheDocument();
    expect(screen.getByText(/A project is a folder on this computer/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open a project folder" })).toBeEnabled();
    // Nothing was read for a project that is not there.
    expect(inspectProject).not.toHaveBeenCalled();
    // The chats, which belong to no project, may be listed; no project's are.
    expect(listConversations).not.toHaveBeenCalledWith(expect.any(String));
    expect(screen.queryByRole("region", { name: "Task summary" })).not.toBeInTheDocument();
  });

  it("puts an interrupted task to the person instead of waiting on it", async () => {
    // A task stopped mid-flight: the shell will produce no more events for
    // it, so following it would leave the window waiting on nothing. What it
    // needs instead is the checkpoint, and a person to decide.
    listTasks.mockResolvedValue([{ ...task, state: "interrupted" as const, lastSequence: 4 }]);
    replayTaskEvents.mockResolvedValue([
      event(1, { kind: "taskCreated", goalSummary: "Existing durable task" }),
      event(2, {
        kind: "taskStateChanged",
        previous: "running",
        current: "interrupted",
        reasonCode: "task.interrupted",
      }),
    ]);
    latestCheckpoint.mockResolvedValue({
      schemaVersion: 1,
      checkpointId: "checkpoint-1",
      taskId: task.taskId,
      eventSequence: 4,
      createdAt: "2026-01-01T00:00:05.000Z",
      taskState: "interrupted",
      completedStepIds: [],
      resume: { eligible: true, mode: "sameWorker", reasonCode: "checkpoint.clean" },
    });

    await renderLoaded();

    const panel = await screen.findByRole("region", { name: "Interrupted" });
    await waitFor(() => {
      expect(panel).toHaveTextContent("Resume");
    });
    expect(latestCheckpoint).toHaveBeenCalledWith(task.taskId);
    expect(watchTask).not.toHaveBeenCalled();
  });

  it("names every control a screen reader reaches, in every view, with no id used twice", async () => {
    // A running task with a question waiting, so the busiest screen is the
    // one checked; then each settings view.
    listTasks.mockResolvedValue([{ ...task, state: "running" as const, lastSequence: 2 }]);
    listPendingApprovals.mockResolvedValue([approval("approval-1")]);
    listConfiguredEndpoints.mockResolvedValue([
      {
        endpointId: "llama",
        displayName: "Local Qwen",
        kind: "llamaServer",
        modelId: "qwen",
        leavesTheDevice: false,
        enabled: true,
        supportsToolCalls: true,
        runtime: { executable: "llama-server", executableStatus: "held" },
      },
    ]);
    const user = userEvent.setup();
    await renderLoaded();
    await screen.findByRole("heading", { name: "Approval needed" });

    expect(unnamedControls()).toEqual([]);
    expect(duplicateIds()).toEqual([]);

    // Every chip's panel, too.
    for (const chip of ["Project", "Where models run", "Access"]) {
      await openChip(user, chip);
      expect(unnamedControls()).toEqual([]);
      expect(duplicateIds()).toEqual([]);
    }

    await openSettings(user, "Models and credentials");
    await screen.findByRole("heading", { name: "Local Qwen" });
    expect(unnamedControls()).toEqual([]);
    expect(duplicateIds()).toEqual([]);

    await openSettings(user, "Customize");
    await screen.findByText(/No Apps are configured/);
    expect(unnamedControls()).toEqual([]);
    expect(duplicateIds()).toEqual([]);
  });

  it("lists each configuration diagnostic with its severity, file and message", async () => {
    inspectProject.mockResolvedValue({
      schemaVersion: 1,
      project,
      configuration: {
        ...configuration,
        diagnostics: [
          {
            relativePath: ".tramontana/project.md",
            message: "front matter is not valid YAML",
            severity: "error",
          },
        ],
      },
    });
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Details" }));
    const list = await screen.findByRole("list", { name: "Configuration diagnostics" });
    expect(within(list).getByRole("listitem")).toHaveTextContent(
      "Error in .tramontana/project.md: front matter is not valid YAML",
    );
  });

  it("reads the standing permissions again once an approval is answered", async () => {
    listPendingApprovals.mockResolvedValue([approval("approval-1")]);
    const user = userEvent.setup();
    await renderLoaded();
    await waitFor(() => {
      expect(listProjectGrants).toHaveBeenCalledTimes(1);
    });

    await user.click(await screen.findByRole("button", { name: "Allow once" }));
    await waitFor(() => {
      expect(listProjectGrants).toHaveBeenCalledTimes(2);
    });
  });

  it("opens the models and credentials in Settings, and goes back to work", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await openSettings(user, "Models and credentials");
    expect(
      await screen.findByRole("heading", { name: "Models and credentials", level: 2 }),
    ).toBeInTheDocument();
    expect(listConfiguredEndpoints).toHaveBeenCalled();
    // Inside Settings there is nothing to go "back" from: the sidebar does it.
    expect(screen.queryByRole("button", { name: "Back to work" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "M2 Conversation" }));
    expect(await screen.findByRole("heading", { name: "M2 Conversation" })).toBeInTheDocument();
  });

  it("surfaces a normalized failure from the boundary", async () => {
    startTask.mockRejectedValue({
      code: "invalidRequest",
      message: "a task needs a goal",
      retryable: false,
    });
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Start task" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "invalidRequest: a task needs a goal",
    );
  });
});
