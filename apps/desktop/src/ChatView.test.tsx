import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ConversationRecord,
  EventEnvelope,
  EventPayload,
  TaskRecord,
  TaskUpdate,
} from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";
import { answerOf, outcomeOf, titleFrom, withEvent } from "./chat.js";

type CoreModule = typeof CoreModuleShape;

const createConversation = vi.hoisted(() => vi.fn());
const listTasks = vi.hoisted(() => vi.fn());
const replayTaskEvents = vi.hoisted(() => vi.fn());
const startTask = vi.hoisted(() => vi.fn());
const cancelTask = vi.hoisted(() => vi.fn());
const watchTask = vi.hoisted(() => vi.fn());
const chooseAttachments = vi.hoisted(() => vi.fn());
const discardAttachment = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return {
    ...actual,
    createConversation,
    listTasks,
    replayTaskEvents,
    startTask,
    cancelTask,
    watchTask,
    chooseAttachments,
    discardAttachment,
  };
});

const { ChatView } = await import("./ChatView.js");

const chat: ConversationRecord = {
  schemaVersion: 1,
  conversationId: "chat-1",
  title: "Winds",
  createdAt: "2026-09-28T00:00:00.000Z",
};

function event(taskId: string, sequence: number, payload: EventPayload): EventEnvelope {
  return {
    schemaVersion: 1,
    eventId: `${taskId}-${sequence}`,
    taskId,
    conversationId: chat.conversationId,
    sequence,
    occurredAt: "2026-09-28T00:00:00.000Z",
    kind: payload.kind as EventEnvelope["kind"],
    visibility: "user",
    correlationId: "c",
    payload,
  };
}

function answer(taskId: string, sequence: number, text: string): EventEnvelope {
  return event(taskId, sequence, {
    kind: "messageCommitted",
    message: {
      schemaVersion: 1,
      messageId: `${taskId}-answer`,
      role: "assistant",
      status: "committed",
      createdAt: "2026-09-28T00:00:01.000Z",
      parts: [{ kind: "text", text }],
    },
  });
}

function task(taskId: string, goal: string, createdAt: string): TaskRecord {
  return {
    schemaVersion: 1,
    taskId,
    conversationId: chat.conversationId,
    goalSummary: goal,
    state: "completed",
    lastSequence: 3,
    createdAt,
    updatedAt: createdAt,
  };
}

function view(conversation: ConversationRecord | undefined, onCreated = vi.fn()) {
  return render(
    <ChatView
      conversation={conversation}
      routingMode="localOnly"
      onRoutingMode={vi.fn()}
      onCreated={onCreated}
      onTurnEnded={vi.fn()}
    />,
  );
}

describe("chat helpers", () => {
  it("reads the last committed answer, and says how a turn ended", () => {
    const events = [
      event("t", 1, { kind: "taskCreated", goalSummary: "Hi?" }),
      answer("t", 2, "Hello."),
      event("t", 3, { kind: "taskCompleted", summary: "Done" }),
    ];
    expect(answerOf(events)).toBe("Hello.");
    expect(outcomeOf(events)).toEqual({ kind: "answered", text: "Hello." });
    expect(outcomeOf(events.slice(0, 1))).toEqual({ kind: "working" });
  });

  it("adds each event to its own turn once, in order", () => {
    const created = event("t", 1, { kind: "taskCreated", goalSummary: "Hi?" });
    let turns = withEvent([], answer("t", 2, "Hello."));
    turns = withEvent(turns, created);
    turns = withEvent(turns, created);
    expect(turns).toHaveLength(1);
    expect(turns[0]?.goal).toBe("Hi?");
    expect(turns[0]?.events.map((each) => each.sequence)).toEqual([1, 2]);
  });

  it("titles a chat from its first line, kept short", () => {
    expect(titleFrom("  What is a bora?\nMore detail")).toBe("What is a bora?");
    expect(titleFrom("x".repeat(80))).toHaveLength(58);
    expect(titleFrom("   ")).toBe("New chat");
  });
});

describe("ChatView", () => {
  beforeEach(() => {
    for (const mock of [
      createConversation,
      listTasks,
      replayTaskEvents,
      startTask,
      cancelTask,
      watchTask,
      chooseAttachments,
      discardAttachment,
    ]) {
      mock.mockReset();
    }
    discardAttachment.mockResolvedValue(undefined);
    watchTask.mockResolvedValue(() => undefined);
  });

  it("makes a chat for the first message, runs it with no tools, and shows the answer", async () => {
    const user = userEvent.setup();
    const onCreated = vi.fn();
    createConversation.mockResolvedValue(chat);
    let sink: (update: TaskUpdate) => void = () => undefined;
    startTask.mockImplementation((_goal: string, onUpdate: (update: TaskUpdate) => void) => {
      sink = onUpdate;
      return Promise.resolve("t1");
    });
    view(undefined, onCreated);

    expect(screen.getByRole("heading", { name: "What can I help with?" })).toBeInTheDocument();
    await user.type(screen.getByLabelText("Message"), "What is a bora?{Enter}");

    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });
    // A chat belongs to no project, and is admitted no tools.
    expect(createConversation).toHaveBeenCalledWith(undefined, "What is a bora?");
    expect(onCreated).toHaveBeenCalledWith(chat);
    const [goal, , conversationId, options] = startTask.mock.calls[0] as unknown[];
    expect(goal).toBe("What is a bora?");
    expect(conversationId).toBe("chat-1");
    expect(options).toMatchObject({
      writes: false,
      recipes: false,
      apps: false,
      browse: false,
      routingMode: "localOnly",
    });

    act(() => {
      sink({
        kind: "event",
        event: event("t1", 1, { kind: "taskCreated", goalSummary: "What is a bora?" }),
      });
      sink({ kind: "frame", frame: { streamId: "s", offset: 0, text: "A cold wind" } });
    });
    expect(await screen.findByRole("region", { name: "Live answer" })).toHaveTextContent(
      "A cold wind",
    );

    act(() => {
      sink({ kind: "event", event: answer("t1", 2, "A cold wind from the north-east.") });
      sink({ kind: "event", event: event("t1", 3, { kind: "taskCompleted", summary: "Done" }) });
    });
    expect(await screen.findByText("A cold wind from the north-east.")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Live answer" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("Message")).toHaveValue("");
  });

  it("shows an existing chat's turns oldest first", async () => {
    listTasks.mockResolvedValue([
      task("t2", "And another?", "2026-09-28T00:02:00.000Z"),
      task("t1", "Name a wind.", "2026-09-28T00:01:00.000Z"),
    ]);
    replayTaskEvents.mockImplementation((taskId: string) =>
      Promise.resolve([
        answer(taskId, 1, taskId === "t1" ? "Bora." : "Jugo."),
        event(taskId, 2, { kind: "taskCompleted", summary: "Done" }),
      ]),
    );
    view(chat);

    const list = await screen.findByRole("list", { name: "Chat messages" });
    await waitFor(() => {
      expect(list).toHaveTextContent(/Name a wind\.Bora\.And another\?Jugo\./);
    });
    expect(watchTask).not.toHaveBeenCalled();
  });

  it("says a failed answer failed, in words", async () => {
    listTasks.mockResolvedValue([task("t1", "Hi?", "2026-09-28T00:01:00.000Z")]);
    replayTaskEvents.mockResolvedValue([
      event("t1", 1, {
        kind: "taskFailed",
        error: {
          schemaVersion: 1,
          code: "providerUnavailable",
          message: "No model is available.",
          retryable: true,
          correlationId: "c",
        },
      }),
    ]);
    view(chat);
    expect(
      await screen.findByText(/This answer failed\. No model is available\./),
    ).toBeInTheDocument();
  });

  it("keeps the picked files when a message fails to start", async () => {
    const user = userEvent.setup();
    listTasks.mockResolvedValue([]);
    chooseAttachments.mockResolvedValue([{ token: "tok-1", name: "plan.md", byteCount: 2048 }]);
    startTask.mockRejectedValue({ code: "conflict", message: "not now", retryable: true });
    view(chat);

    await user.click(screen.getByRole("button", { name: "Attach files" }));
    await screen.findByRole("list", { name: "Attached files" });
    await user.type(screen.getByLabelText("Message"), "Summarize it.{Enter}");

    expect(await screen.findByRole("alert")).toHaveTextContent("conflict: not now");
    expect(screen.getByRole("list", { name: "Attached files" })).toHaveTextContent("plan.md");
  });

  it("sends the files the person picked, and shows them on the message", async () => {
    const user = userEvent.setup();
    listTasks.mockResolvedValue([]);
    chooseAttachments.mockResolvedValue([
      { token: "tok-1", name: "plan.md", byteCount: 2048 },
      { token: "tok-2", name: "costs.csv", byteCount: 90 },
    ]);
    let sink: (update: TaskUpdate) => void = () => undefined;
    startTask.mockImplementation((_goal: string, onUpdate: (update: TaskUpdate) => void) => {
      sink = onUpdate;
      return Promise.resolve("t1");
    });
    view(chat);

    await user.click(screen.getByRole("button", { name: "Attach files" }));
    const pending = await screen.findByRole("list", { name: "Attached files" });
    expect(pending).toHaveTextContent("plan.md");
    expect(pending).toHaveTextContent("2 KB");

    // Removed before sending: the desktop stops holding it.
    await user.click(screen.getByRole("button", { name: "Remove costs.csv" }));
    expect(discardAttachment).toHaveBeenCalledWith("tok-2");

    await user.type(screen.getByLabelText("Message"), "Summarize it.{Enter}");
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });
    const [, , , options] = startTask.mock.calls[0] as unknown[];
    expect(options).toMatchObject({ attachments: ["tok-1"] });
    await waitFor(() => {
      expect(screen.queryByRole("list", { name: "Attached files" })).not.toBeInTheDocument();
    });

    act(() => {
      sink({
        kind: "event",
        event: event("t1", 1, {
          kind: "taskCreated",
          goalSummary: "Summarize it.",
          attachments: [
            {
              name: "plan.md",
              mediaType: "text/markdown",
              text: "# Plan",
              contentHash: "h",
              byteCount: 2048,
            },
          ],
        }),
      });
    });
    expect(
      await screen.findByRole("list", { name: "Files sent with this message" }),
    ).toHaveTextContent("plan.md");
  });

  it("says why a file could not be attached", async () => {
    const user = userEvent.setup();
    chooseAttachments.mockRejectedValue({
      schemaVersion: 1,
      code: "invalidRequest",
      message: "contract.pdf is not a text file Tramontana can read yet",
      retryable: false,
      correlationId: "c",
    });
    view(undefined);
    await user.click(screen.getByRole("button", { name: "Attach files" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("contract.pdf is not a text file");
  });
});
