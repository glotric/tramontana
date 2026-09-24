import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ReplaySafety, TaskCheckpoint } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const latestCheckpoint = vi.hoisted(() => vi.fn());
const resumeTask = vi.hoisted(() => vi.fn());
const discardCheckpoint = vi.hoisted(() => vi.fn());
const handoffOptions = vi.hoisted(() => vi.fn());
const handOffTask = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return {
    ...actual,
    latestCheckpoint,
    resumeTask,
    discardCheckpoint,
    handoffOptions,
    handOffTask,
  };
});

const { CheckpointPanel } = await import("./CheckpointPanel.js");

/**
 * A checkpoint with, or deliberately without, a pending action.
 *
 * The field is omitted rather than set to `undefined`: the contract says an
 * absent value is absent, and the project compiles with
 * `exactOptionalPropertyTypes`, so the two are not the same thing here either.
 */
function checkpoint(safety?: ReplaySafety, eligible = true): TaskCheckpoint {
  const base: TaskCheckpoint = {
    schemaVersion: 1,
    checkpointId: "checkpoint-1",
    taskId: "task-1",
    eventSequence: 12,
    createdAt: "2026-01-01T00:00:00.000Z",
    taskState: "interrupted",
    completedStepIds: ["read-the-readme"],
    resume: {
      eligible,
      mode: "sameWorker",
      reasonCode: eligible ? "checkpoint.clean" : "checkpoint.workerNotResumable",
    },
  };
  if (safety === undefined) return base;
  return {
    ...base,
    pendingAction: {
      kind: "tool",
      referenceId: "invocation-2",
      replaySafety: safety,
      displaySummary: "builtin.fs.write started without a recorded result",
    },
  };
}

describe("the checkpoint inspector", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resumeTask.mockResolvedValue(undefined);
    discardCheckpoint.mockResolvedValue(undefined);
    handOffTask.mockResolvedValue(undefined);
    handoffOptions.mockResolvedValue({ workers: [], escalating: false });
  });

  it("says why the worker stopped and hands the task to another, never back to itself", async () => {
    const onChanged = vi.fn();
    const user = userEvent.setup();
    latestCheckpoint.mockResolvedValue({
      ...checkpoint(),
      resume: { eligible: true, mode: "canonicalHandoff" },
      worker: {
        descriptor: {
          agentId: "builtin.reference@local/qwen",
          agentProviderId: "builtin",
          checkpointSupport: "portable",
          displayName: "Embedded read-only worker on Qwen",
          integrationMode: "embedded",
          localOnlyEligible: true,
          modelControl: "tramontanaBrokered",
          networkControl: "denied",
          toolControl: "tramontanaBrokered",
        },
      },
    } satisfies TaskCheckpoint);
    // Already ordered and filtered by the core: the worker that has the task
    // is not among them (ADR 0055).
    handoffOptions.mockResolvedValue({
      workers: [
        { agentId: "builtin.reference@local/qwen7", displayName: "Qwen 7B", cloud: false },
        { agentId: "builtin.reference", displayName: "Automatic" },
        { agentId: "builtin.reference@cloud/claude", displayName: "Claude", cloud: true },
      ],
      escalating: false,
    });
    render(
      <CheckpointPanel
        taskId="task-1"
        stoppedBecause="The worker used up its budget before finishing."
        onChanged={onChanged}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "Stopped: the worker could not go on" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/used up its budget/)).toBeInTheDocument();
    const choice = await screen.findByRole("combobox", { name: "Hand over to" });
    const offered = [...(choice as HTMLSelectElement).options].map((option) => option.text);
    // Rendered in the order the core gave, not one this component invented.
    expect(offered).toEqual(["Qwen 7B", "Automatic", "Claude (off this device)"]);
    expect(handoffOptions).toHaveBeenCalledWith("task-1");
    expect(screen.getByText(/Hand it to Qwen 7B to continue from here/)).toBeVisible();

    await user.selectOptions(choice, "builtin.reference@cloud/claude");
    await user.click(screen.getByRole("button", { name: "Hand over" }));
    await waitFor(() => {
      expect(handOffTask).toHaveBeenCalledWith(
        "task-1",
        "checkpoint-1",
        "builtin.reference@cloud/claude",
      );
    });
    expect(onChanged).toHaveBeenCalled();
  });

  it("offers no handoff when there is nobody else to hand to", async () => {
    latestCheckpoint.mockResolvedValue(checkpoint());
    render(<CheckpointPanel taskId="task-1" onChanged={vi.fn()} />);
    expect(await screen.findByText("checkpoint-1")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Interrupted" })).toBeInTheDocument();
    await waitFor(() => {
      expect(handoffOptions).toHaveBeenCalled();
    });
    expect(screen.queryByRole("button", { name: "Hand over" })).not.toBeInTheDocument();
  });

  it("says why the cloud is offered first, and what handing it there would send", async () => {
    latestCheckpoint.mockResolvedValue({
      ...checkpoint(),
      resume: { eligible: true, mode: "canonicalHandoff" },
    } satisfies TaskCheckpoint);
    handoffOptions.mockResolvedValue({
      workers: [
        { agentId: "builtin.reference@cloud/claude", displayName: "Claude", cloud: true },
        { agentId: "builtin.reference@local/qwen7", displayName: "Qwen 7B", cloud: false },
      ],
      escalating: true,
    });
    render(
      <CheckpointPanel
        taskId="task-1"
        stoppedBecause="The worker used up its budget before finishing."
        onChanged={vi.fn()}
      />,
    );

    const choice = await screen.findByRole("combobox", { name: "Hand over to" });
    expect([...(choice as HTMLSelectElement).options][0]?.text).toBe("Claude (off this device)");
    // The reason and the cost, before the button rather than after it.
    expect(screen.getByText(/ran out of what its model could do/)).toBeVisible();
    expect(screen.getByText(/sends the task.s record/)).toBeVisible();
    expect(screen.getByText(/nothing moves until you click/)).toBeVisible();
  });

  it("does not offer to escalate a task that merely lost its provider", async () => {
    latestCheckpoint.mockResolvedValue({
      ...checkpoint(),
      resume: { eligible: true, mode: "canonicalHandoff" },
    } satisfies TaskCheckpoint);
    handoffOptions.mockResolvedValue({
      workers: [
        { agentId: "builtin.reference@local/qwen7", displayName: "Qwen 7B", cloud: false },
        { agentId: "builtin.reference@cloud/claude", displayName: "Claude", cloud: true },
      ],
      escalating: false,
    });
    render(
      <CheckpointPanel
        taskId="task-1"
        stoppedBecause="The model endpoint could not be reached."
        onChanged={vi.fn()}
      />,
    );

    await screen.findByRole("combobox", { name: "Hand over to" });
    expect(screen.queryByText(/ran out of what its model could do/)).toBeNull();
  });

  it("shows what was in flight and how safe repeating it is", async () => {
    latestCheckpoint.mockResolvedValue(checkpoint("unsafe"));
    render(<CheckpointPanel taskId="task-1" onChanged={vi.fn()} />);

    expect(await screen.findByText(/builtin\.fs\.write/)).toBeInTheDocument();
    expect(screen.getByText(/could duplicate an effect/i)).toBeInTheDocument();
    expect(screen.getByText(/event 12/)).toBeInTheDocument();
    // The watermark is stated, because "resume will not repeat these" is the
    // reassurance a person actually needs before pressing anything.
    expect(screen.getByText(/1 \(a resume will not repeat them\)/)).toBeInTheDocument();
  });

  it("refuses to offer resume when the pending action may have had an effect", async () => {
    for (const unsafe of ["unsafe", "unknown"] as const) {
      latestCheckpoint.mockResolvedValue(checkpoint(unsafe));
      const { unmount } = render(<CheckpointPanel taskId="task-1" onChanged={vi.fn()} />);

      await waitFor(() => {
        expect(screen.getByRole("button", { name: "Resume" })).toBeDisabled();
      });
      // Discard stays available: the person must be able to decide either way.
      expect(screen.getByRole("button", { name: "Discard" })).toBeEnabled();
      unmount();
    }
  });

  it("offers resume when nothing was in flight", async () => {
    latestCheckpoint.mockResolvedValue(checkpoint(undefined));
    const onChanged = vi.fn();
    render(<CheckpointPanel taskId="task-1" onChanged={onChanged} />);

    const resume = await screen.findByRole("button", { name: "Resume" });
    expect(resume).toBeEnabled();
    await userEvent.click(resume);

    // The checkpoint on screen is the one resumed, never "the latest".
    expect(resumeTask).toHaveBeenCalledWith("task-1", "checkpoint-1");
    await waitFor(() => {
      expect(onChanged).toHaveBeenCalled();
    });
  });

  it("offers resume for a safe pending action", async () => {
    latestCheckpoint.mockResolvedValue(checkpoint("safe"));
    render(<CheckpointPanel taskId="task-1" onChanged={vi.fn()} />);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Resume" })).toBeEnabled();
    });
    expect(screen.getByText(/changes nothing outside Tramontana/i)).toBeInTheDocument();
  });

  it("explains an ineligible checkpoint with the reason the core gave", async () => {
    latestCheckpoint.mockResolvedValue(checkpoint(undefined, false));
    render(<CheckpointPanel taskId="task-1" onChanged={vi.fn()} />);

    expect(await screen.findByText(/checkpoint\.workerNotResumable/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Resume" })).toBeDisabled();
  });

  it("surfaces the core's refusal rather than a generic failure", async () => {
    latestCheckpoint.mockResolvedValue(checkpoint(undefined));
    resumeTask.mockRejectedValue({
      schemaVersion: 1,
      code: "conflict",
      message: "Checkpoint is stale; create and inspect a new checkpoint",
      retryable: false,
      correlationId: "correlation-1",
    });
    render(<CheckpointPanel taskId="task-1" onChanged={vi.fn()} />);

    await userEvent.click(await screen.findByRole("button", { name: "Resume" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/Checkpoint is stale/);
  });

  it("says discarding keeps history and undoes nothing", async () => {
    latestCheckpoint.mockResolvedValue(checkpoint(undefined));
    const onChanged = vi.fn();
    render(<CheckpointPanel taskId="task-1" onChanged={onChanged} />);

    expect(
      await screen.findByText(/keeps the history and undoes nothing that already happened/i),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(discardCheckpoint).toHaveBeenCalledWith("task-1");
    await waitFor(() => {
      expect(onChanged).toHaveBeenCalled();
    });
  });

  it("says so plainly when a stopped task left no checkpoint", async () => {
    latestCheckpoint.mockResolvedValue(undefined);
    render(<CheckpointPanel taskId="task-1" onChanged={vi.fn()} />);

    expect(await screen.findByText(/nothing to resume from/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Resume" })).not.toBeInTheDocument();
  });
});
