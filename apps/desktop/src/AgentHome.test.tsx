import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectRecord, TaskRecord, TaskState } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const listTasks = vi.hoisted(() => vi.fn());
vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, listTasks };
});

const { AgentHome } = await import("./AgentHome.js");

const harbor: ProjectRecord = {
  schemaVersion: 1,
  projectId: "p1",
  name: "harbor",
  roots: ["/work/harbor"],
  createdAt: "2026-01-01",
};

function task(taskId: string, state: TaskState, projectId?: string): TaskRecord {
  const at = new Date().toISOString();
  return {
    schemaVersion: 1,
    taskId,
    goalSummary: `Goal of ${taskId}`,
    state,
    lastSequence: 1,
    ...(projectId === undefined ? {} : { projectId, conversationId: `conversation-${taskId}` }),
    createdAt: at,
    updatedAt: at,
  };
}

/** A tile's number, found by its label among the tiles. */
function tile(label: string): string {
  const term = [...document.querySelectorAll(".overview-tiles dt")].find(
    (dt) => dt.textContent === label,
  );
  return term?.nextElementSibling?.textContent ?? "";
}

beforeEach(() => {
  listTasks.mockReset();
});

describe("AgentHome", () => {
  it("lists a task still running, and opens its conversation", async () => {
    // A window opened on the home screen used to give no sign of a task
    // still running in the shell.
    const user = userEvent.setup();
    const onOpenConversation = vi.fn();
    listTasks.mockResolvedValue([
      task("running", "running", "p1"),
      task("done", "completed", "p1"),
    ]);
    render(
      <AgentHome
        name="Ada"
        projects={[harbor]}
        revision={0}
        onOpenProject={() => undefined}
        canOpenProject
        onOpenConversation={onOpenConversation}
      />,
    );

    expect(screen.getByRole("heading", { name: "What's up next, Ada?", level: 2 })).toBeTruthy();
    const running = await screen.findByRole("region", { name: "Still running" });
    expect(running).toHaveTextContent("Goal of running");
    expect(running).toHaveTextContent("harbor");
    expect(running).not.toHaveTextContent("Goal of done");

    await user.click(screen.getByRole("button", { name: /Goal of running/ }));
    expect(onOpenConversation).toHaveBeenCalledWith("p1", "conversation-running");
  });

  it("counts Agent tasks only, leaving chats to Chat", async () => {
    listTasks.mockResolvedValue([
      task("one", "completed", "p1"),
      task("two", "failed", "p1"),
      task("chat", "completed"),
    ]);
    render(
      <AgentHome
        name={undefined}
        projects={[harbor]}
        revision={0}
        onOpenProject={() => undefined}
        canOpenProject
        onOpenConversation={() => undefined}
      />,
    );

    await waitFor(() => {
      expect(tile("Tasks")).toBe("2");
    });
    expect(screen.queryByRole("region", { name: "Still running" })).not.toBeInTheDocument();
  });
});
