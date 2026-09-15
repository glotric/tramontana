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

describe("M2 durable desktop surface", () => {
  beforeEach(() => {
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
    ]) {
      mock.mockReset();
    }
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

  it("opens a native-picked project and refreshes the durable recents", async () => {
    const user = userEvent.setup();
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "+ Open project" }));
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
        { writes: false, recipes: false, routingMode: "automatic" },
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
        { writes: true, recipes: true, routingMode: "localOnly" },
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
