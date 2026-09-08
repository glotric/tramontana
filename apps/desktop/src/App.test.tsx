import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EventEnvelope, EventPayload, TaskUpdate } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const fetchCoreInfo = vi.hoisted(() => vi.fn());
const startTask = vi.hoisted(() => vi.fn());
const cancelTask = vi.hoisted(() => vi.fn());
vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, fetchCoreInfo, startTask, cancelTask };
});

const { App } = await import("./App.js");

function event(sequence: number, payload: EventPayload): EventEnvelope {
  return {
    schemaVersion: 1,
    eventId: `event-${sequence}`,
    taskId: "task-1",
    sequence,
    occurredAt: "2026-01-01T00:00:00.000Z",
    kind: payload.kind as EventEnvelope["kind"],
    visibility: "user",
    correlationId: "c1",
    payload,
  };
}

/** Capture the update callback so a test can drive the stream by hand. */
function captureUpdates(): { emit: (update: TaskUpdate) => void } {
  let sink: (update: TaskUpdate) => void = () => undefined;
  startTask.mockImplementation((_goal: string, onUpdate: (u: TaskUpdate) => void) => {
    sink = onUpdate;
    return Promise.resolve("task-1");
  });
  return {
    emit: (update) => {
      sink(update);
    },
  };
}

describe("M1 walking skeleton", () => {
  beforeEach(() => {
    fetchCoreInfo.mockReset();
    startTask.mockReset();
    cancelTask.mockReset();
    fetchCoreInfo.mockResolvedValue({ schemaVersion: 1, appVersion: "0.1.0" });
    cancelTask.mockResolvedValue(undefined);
  });

  it("reports a compatible core once the handshake resolves", async () => {
    render(<App />);
    expect(await screen.findByText(/core 0\.1\.0 · contracts v1/i)).toBeInTheDocument();
  });

  it("reports a core whose contract version this build does not speak", async () => {
    fetchCoreInfo.mockResolvedValue({ schemaVersion: 99, appVersion: "0.1.0" });
    render(<App />);
    expect(await screen.findByText(/speaks contracts v99/i)).toBeInTheDocument();
  });

  it("stays usable when the core is unreachable outside the desktop shell", async () => {
    fetchCoreInfo.mockRejectedValue(new Error("no ipc"));
    render(<App />);
    expect(await screen.findByText(/core unavailable/i)).toBeInTheDocument();
  });

  it("renders the event stream in order as updates arrive", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalledWith("Say hello", expect.any(Function));
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

    const list = await screen.findByRole("list");
    const rendered = within(list).getAllByRole("listitem");
    expect(rendered).toHaveLength(2);
    expect(rendered[0]).toHaveTextContent("goal: Say hello");
    expect(rendered[1]).toHaveTextContent("queued → running");
  });

  it("ignores a duplicate sequence so a replayed event is not rendered twice", async () => {
    // Subscription precedes replay on the Rust side, so the same event can
    // legitimately arrive twice. Showing it twice would misrepresent the record.
    const updates = captureUpdates();
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });

    const created = event(1, { kind: "taskCreated", goalSummary: "Say hello" });
    updates.emit({ kind: "event", event: created });
    updates.emit({ kind: "event", event: created });

    const list = await screen.findByRole("list");
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
  });

  it("shows streamed frames separately from the durable event list", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });

    updates.emit({ kind: "event", event: event(1, { kind: "taskCreated", goalSummary: "x" }) });
    updates.emit({ kind: "frame", frame: { streamId: "s", offset: 0, text: "Hel" } });
    updates.emit({ kind: "frame", frame: { streamId: "s", offset: 3, text: "lo" } });

    expect(await screen.findByText(/Hello/)).toBeInTheDocument();
    // Frames are transport, not durable record: they must not become list items.
    const list = screen.getByRole("list");
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
  });

  it("enables Stop only while a task is running", async () => {
    const updates = captureUpdates();
    const user = userEvent.setup();
    render(<App />);

    const stop = screen.getByRole("button", { name: "Stop" });
    expect(stop).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "Start task" }));
    await waitFor(() => {
      expect(startTask).toHaveBeenCalled();
    });
    updates.emit({ kind: "event", event: event(1, { kind: "taskCreated", goalSummary: "x" }) });
    await waitFor(() => {
      expect(stop).toBeEnabled();
    });

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
      expect(stop).toBeDisabled();
    });
  });

  it("surfaces a normalized failure from the boundary", async () => {
    startTask.mockRejectedValue({
      code: "invalidRequest",
      message: "a task needs a goal",
      retryable: false,
    });
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "Start task" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "invalidRequest: a task needs a goal",
    );
  });
});
