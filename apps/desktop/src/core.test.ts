import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const Channel = vi.hoisted(() =>
  vi.fn(function (this: { onmessage?: unknown }) {
    this.onmessage = undefined;
  }),
);
vi.mock("@tauri-apps/api/core", () => ({ invoke, Channel }));

const { asErrorEnvelope, cancelTask, fetchCoreInfo, getTask, startTask } =
  await import("./core.js");

describe("core IPC client", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue({ schemaVersion: 1, appVersion: "0.1.0" });
  });

  it("invokes the command names the Rust shell registers", async () => {
    // Command names are strings here and identifiers in Rust, so nothing at
    // compile time links them. `tauri::generate_handler!` registers these four;
    // a rename on either side must fail here rather than at runtime as a silent
    // "core unavailable".
    await fetchCoreInfo();
    expect(invoke).toHaveBeenCalledWith("core_info");

    invoke.mockResolvedValue("task-1");
    await startTask("hello", () => undefined);
    expect(invoke).toHaveBeenCalledWith("start_task", expect.objectContaining({ goal: "hello" }));

    invoke.mockResolvedValue(undefined);
    await cancelTask("task-1");
    expect(invoke).toHaveBeenCalledWith("cancel_task", { taskId: "task-1" });

    invoke.mockResolvedValue({
      schemaVersion: 1,
      taskId: "task-1",
      state: "queued",
      lastSequence: 0,
    });
    await getTask("task-1");
    expect(invoke).toHaveBeenCalledWith("get_task", { taskId: "task-1" });
  });

  it("wires the update callback to the channel it passes across the boundary", async () => {
    invoke.mockResolvedValue("task-1");
    const onUpdate = vi.fn();
    await startTask("hello", onUpdate);

    const payload = invoke.mock.calls.at(-1)?.[1] as {
      channel: { onmessage: (u: unknown) => void };
    };
    const channel = payload.channel;
    channel.onmessage({ kind: "frame", frame: { streamId: "s", offset: 0, text: "hi" } });
    expect(onUpdate).toHaveBeenCalledWith({
      kind: "frame",
      frame: { streamId: "s", offset: 0, text: "hi" },
    });
  });

  it("recognizes a normalized error and rejects anything else", () => {
    expect(
      asErrorEnvelope({ code: "notFound", message: "unknown task", retryable: false }),
    ).toMatchObject({ code: "notFound" });

    // A thrown string or a plain Error is not a contract error; treating one as
    // if it were would put unredacted text in front of the user.
    expect(asErrorEnvelope("boom")).toBeUndefined();
    expect(asErrorEnvelope(new Error("boom"))).toBeUndefined();
    expect(asErrorEnvelope(null)).toBeUndefined();
  });
});
