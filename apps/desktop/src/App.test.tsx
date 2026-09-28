import { act, render, screen, waitFor, within } from "@testing-library/react";
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

async function renderLoaded(): Promise<void> {
  render(<App />);
  await screen.findByRole("heading", { name: "M2 Conversation" });
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
    ]) {
      mock.mockReset();
    }
    watchTask.mockResolvedValue(undefined);
    latestCheckpoint.mockResolvedValue(undefined);
    listApps.mockResolvedValue([]);
    listWorkers.mockResolvedValue([]);
    listSkills.mockResolvedValue([]);
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
    await renderLoaded();

    expect(screen.getByText("/work/tramontana")).toBeInTheDocument();
    expect(screen.getByText("Existing durable task")).toBeInTheDocument();
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

    await user.click(screen.getByRole("button", { name: "Agent" }));
    expect(await screen.findByRole("heading", { name: "M2 Conversation" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Agent" })).toHaveAttribute("aria-pressed", "true");
    expect(localStorage.getItem("tramontana.mode")).toBe("agent");

    await user.click(screen.getByRole("button", { name: "Chat" }));
    expect(screen.getByRole("heading", { name: "New chat", level: 1 })).toBeInTheDocument();
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

    // Open, the panel closes from its own button, and the pill is gone.
    expect(screen.queryByRole("button", { name: "Details" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Close details" }));
    expect(
      screen.queryByRole("complementary", { name: "Project inspector" }),
    ).not.toBeInTheDocument();
    // Closed, the pill brings it back.
    await user.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByRole("complementary", { name: "Project inspector" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Details" })).not.toBeInTheDocument();
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

    const stop = screen.getByRole("button", { name: "Stop" });
    expect(stop).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });
    updates.emit({
      kind: "event",
      event: event(1, { kind: "taskCreated", goalSummary: "Say hello" }),
    });
    await waitFor(() => expect(stop).toBeEnabled());

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
    await waitFor(() => expect(stop).toBeDisabled());
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

    await user.click(screen.getByRole("radio", { name: "Local only" }));
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

    const choice = await screen.findByRole("combobox", { name: "Worker" });
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
    await user.click(screen.getByRole("radio", { name: "Local only" }));
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

  it("gives a task the project's skills the person ticked, and none otherwise", async () => {
    listSkills.mockResolvedValue([
      { id: "review", displayName: "Review Rust", description: "Look for unsafe code." },
      { id: "release", displayName: "Release checklist" },
    ]);
    const user = userEvent.setup();
    await renderLoaded();

    // Offering is not selecting: nothing is ticked until the person ticks it.
    await screen.findByText(/Review Rust — Look for unsafe code\./);
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        expect.not.objectContaining({ skills: expect.anything() }),
      );
    });

    await user.click(screen.getByRole("checkbox", { name: /Review Rust/ }));
    expect(screen.getByText(/It is given one of the project's skills to follow\./)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenLastCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        expect.objectContaining({ skills: ["review"] }),
      );
    });
  });

  it("marks the skills that match the goal and shows them first, ticking none", async () => {
    listSkills.mockResolvedValue([
      { id: "release", displayName: "Release checklist" },
      { id: "rust-quality", displayName: "Rust quality pass", description: "Format and lint." },
    ]);
    suggestSkills.mockResolvedValue(["rust-quality"]);
    const user = userEvent.setup();
    await renderLoaded();

    const marked = await screen.findByText("suggested for this goal");
    // Said in words, and put first, without ticking anything.
    const boxes = screen.getAllByRole<HTMLInputElement>("checkbox", { name: /pass|checklist/ });
    expect(boxes.map((box) => box.checked)).toEqual([false, false]);
    expect(marked.closest("label")?.textContent).toContain("Rust quality pass");
    const names = screen
      .getAllByRole("checkbox", { name: /pass|checklist/ })
      .map((box) => box.closest("label")?.textContent ?? "");
    expect(names[0]).toContain("Rust quality pass");
    expect(names[1]).toContain("Release checklist");

    // Suggesting is not selecting: the task carries nothing until it is ticked.
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith(
        "Say hello",
        expect.any(Function),
        conversation.conversationId,
        expect.not.objectContaining({ skills: expect.anything() }),
      );
    });
  });

  it("shows no skills chooser for a project that offers none", async () => {
    await renderLoaded();
    expect(screen.queryByRole("group", { name: "Skills this project offers" })).toBeNull();
  });

  it("lets a task browse only when the person allows it, and says what Local only does not cover", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("radio", { name: "Local only" }));
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
    render(<App />);

    expect(await screen.findByRole("button", { name: "Import a bundle…" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Export project…" })).not.toBeInTheDocument();
  });

  it("says what is selected and what state a task is in, not by color alone", async () => {
    replayTaskEvents.mockResolvedValue([
      event(1, { kind: "taskCreated", goalSummary: "Existing durable task" }),
    ]);
    const { container } = render(<App />);
    await screen.findByRole("heading", { name: "M2 Conversation" });

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
        return Promise.resolve();
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
        return Promise.resolve();
      },
    );

    const first = render(<App />);
    await screen.findByRole("heading", { name: "M2 Conversation" });
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
    await screen.findByRole("heading", { name: "M2 Conversation" });
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

    expect(await screen.findByText("No durable projects yet.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open project" })).toBeEnabled();
    expect(screen.getByRole("heading", { name: "Open a project", level: 1 })).toBeInTheDocument();
    expect(screen.getByText(/Choose a local directory/)).toBeInTheDocument();
    // Nothing was read for a project that is not there.
    expect(inspectProject).not.toHaveBeenCalled();
    expect(listConversations).not.toHaveBeenCalled();
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
    expect(panel).toHaveTextContent("Resume");
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

    await user.click(screen.getByRole("button", { name: "Models and credentials" }));
    await screen.findByRole("heading", { name: "Local Qwen" });
    expect(unnamedControls()).toEqual([]);
    expect(duplicateIds()).toEqual([]);

    await user.click(screen.getByRole("button", { name: "Apps" }));
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
    await renderLoaded();

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

  it("opens the models and credentials, and goes back to work", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Models and credentials" }));
    expect(
      await screen.findByRole("heading", { name: "Models and credentials" }),
    ).toBeInTheDocument();
    expect(listConfiguredEndpoints).toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Back to work" }));
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
