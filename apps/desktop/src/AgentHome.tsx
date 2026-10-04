import { useEffect, useState } from "react";
import type { ProjectRecord, TaskRecord } from "@tramontana/shared";

import { isTerminal } from "./activity.js";
import { ActivityOverview } from "./ActivityOverview.js";
import { listTasks } from "./core.js";
import { Glyph } from "./Glyph.js";

/** How often the list of running tasks is read again while it has any. */
const RUNNING_POLL_MS = 5_000;

/** A task still under way in a project's conversation. */
type RunningTask = TaskRecord & { readonly projectId: string; readonly conversationId: string };

function isRunning(task: TaskRecord): task is RunningTask {
  return (
    task.projectId !== undefined &&
    task.conversationId !== undefined &&
    !isTerminal(task.state) &&
    task.state !== "interrupted"
  );
}

/**
 * Agent mode with no conversation open: a greeting, any task still running,
 * what this profile's Agent tasks add up to, and — when there is no project
 * yet — the way to open one. The composer below starts the next task.
 */
export function AgentHome({
  name,
  projects,
  revision,
  onOpenProject,
  canOpenProject,
  onOpenConversation,
}: {
  readonly name: string | undefined;
  readonly projects: readonly ProjectRecord[];
  /** Moves on when a task ends, so the counts are read again. */
  readonly revision: number;
  readonly onOpenProject: () => void;
  readonly canOpenProject: boolean;
  /** Open the conversation a running task is in. */
  readonly onOpenConversation: (projectId: string, conversationId: string) => void;
}): React.JSX.Element {
  // Agent tasks only: a chat has no project, and is counted in Chat.
  const [tasks, setTasks] = useState<readonly TaskRecord[] | undefined>();
  // Moves on to read the records again while a task is running, so one that
  // ends leaves the list without the window having to watch it.
  const [polls, setPolls] = useState(0);

  useEffect(() => {
    let active = true;
    void listTasks().then(
      (found) => {
        if (active) setTasks(found.filter((task) => task.projectId !== undefined));
      },
      () => {
        if (active) setTasks([]);
      },
    );
    return () => {
      active = false;
    };
  }, [revision, polls]);

  const running = (tasks ?? []).filter(isRunning);
  const anyRunning = running.length > 0;
  useEffect(() => {
    if (!anyRunning) return undefined;
    const timer = setTimeout(() => {
      setPolls((current) => current + 1);
    }, RUNNING_POLL_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [anyRunning, tasks]);

  const greeting =
    name === undefined || name.trim() === "" ? "What's up next?" : `What's up next, ${name}?`;

  return (
    <section className="agent-home" aria-labelledby="agent-home-heading">
      {/* The page's own heading is the header's "New task"; this one is
          second to it. */}
      <h2 id="agent-home-heading" className="agent-home-greeting">
        <Glyph icon="wind" hue="blue" size={40} />
        {greeting}
      </h2>
      {anyRunning && (
        <section className="agent-home-running" aria-labelledby="agent-home-running-heading">
          <h3 id="agent-home-running-heading">Still running</h3>
          <ul>
            {running.map((task) => (
              <li key={task.taskId}>
                <button
                  type="button"
                  onClick={() => {
                    onOpenConversation(task.projectId, task.conversationId);
                  }}
                >
                  <span className="status-dot" data-state="ready" aria-hidden="true" />
                  <span className="agent-home-running-goal">{task.goalSummary}</span>
                  <span className="agent-home-running-project">
                    {projects.find((project) => project.projectId === task.projectId)?.name ?? ""}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      {projects.length === 0 ? (
        <div className="agent-home-start">
          <p>
            A project is a folder on this computer that tasks can work in. Its Markdown is read as
            configuration and never run.
          </p>
          <button
            type="button"
            className="button button--primary"
            onClick={onOpenProject}
            disabled={!canOpenProject}
          >
            Open a project folder
          </button>
        </div>
      ) : (
        <ActivityOverview tasks={tasks} projects={projects} />
      )}
    </section>
  );
}
