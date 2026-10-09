import type { EventEnvelope } from "@tramontana/shared";

/**
 * The commands a task ran, as a terminal shows them: what was run, where it
 * stands, and what it printed. Read from the task's own record, so the
 * window shows exactly what happened and nothing it was not told.
 */

/** The tools that run a command on this computer. */
const COMMAND_TOOLS = new Set(["builtin.recipe.run"]);

export type CommandStatus = "waiting" | "refused" | "running" | "done" | "failed";

export interface CommandRun {
  readonly invocationId: string;
  /** What was asked to run, as the record summarizes it. */
  readonly command: string;
  readonly status: CommandStatus;
  readonly exitCode?: number | null;
  readonly stdout?: string;
  readonly stderr?: string;
  /** Output was cut short where it was kept. */
  readonly truncated?: boolean;
  /** Why it failed or was refused, in words. */
  readonly note?: string;
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

export function commandRuns(events: readonly EventEnvelope[]): readonly CommandRun[] {
  const runs: CommandRun[] = [];
  const at = new Map<string, number>();
  const update = (id: string, change: (run: CommandRun) => CommandRun): void => {
    const index = at.get(id);
    const run = index === undefined ? undefined : runs[index];
    if (index !== undefined && run !== undefined) runs[index] = change(run);
  };

  for (const { payload } of events) {
    switch (payload.kind) {
      case "toolRequested":
        if (!COMMAND_TOOLS.has(payload.toolId)) break;
        at.set(payload.invocationId, runs.length);
        runs.push({
          invocationId: payload.invocationId,
          command: payload.argumentsSummary,
          status: "waiting",
        });
        break;
      case "permissionDecided":
        if (!payload.allowed) {
          update(payload.invocationId, (run) => ({
            ...run,
            status: "refused",
            note: "Not allowed to run.",
          }));
        }
        break;
      case "toolStarted":
        update(payload.invocationId, (run) => ({ ...run, status: "running" }));
        break;
      case "toolCompleted":
        update(payload.invocationId, (run) => ({
          ...run,
          status: "done",
          note: payload.displaySummary,
        }));
        break;
      case "toolFailed":
        update(payload.invocationId, (run) => ({
          ...run,
          status: "failed",
          note: payload.error.message,
        }));
        break;
      case "messageCommitted":
        for (const part of payload.message.parts) {
          if (part.kind !== "toolResult" || !at.has(part.invocationId)) continue;
          const content = part.content;
          const exit = content["exitCode"];
          const stdout = text(content["stdout"]);
          const stderr = text(content["stderr"]);
          update(part.invocationId, (run) => ({
            ...run,
            exitCode: typeof exit === "number" ? exit : null,
            ...(stdout === undefined ? {} : { stdout }),
            ...(stderr === undefined ? {} : { stderr }),
            truncated: content["truncated"] === true || part.truncated,
          }));
        }
        break;
      default:
        break;
    }
  }
  // A command that ran to the end but exited non-zero failed, whichever of
  // its events came first.
  return runs.map((run) =>
    run.status === "done" && typeof run.exitCode === "number" && run.exitCode !== 0
      ? { ...run, status: "failed" }
      : run,
  );
}

/** Whether any command is waiting or running: the moment to show the terminal. */
export function commandUnderway(runs: readonly CommandRun[]): boolean {
  return runs.some((run) => run.status === "waiting" || run.status === "running");
}
