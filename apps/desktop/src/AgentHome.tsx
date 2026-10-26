import { useState } from "react";
import type { ProjectRecord, TaskRecord } from "@tramontana/shared";

import { ActivityOverview } from "./ActivityOverview.js";
import { listTasks } from "./core.js";
import { Glyph } from "./Glyph.js";
import { useLoad } from "./useLoad.js";

/**
 * Agent mode with no conversation open: a greeting, what this profile's
 * tasks add up to, and — when there is no project yet — the way to open
 * one. The composer below starts the next task.
 */
export function AgentHome({
  name,
  projects,
  revision,
  onOpenProject,
  canOpenProject,
}: {
  readonly name: string | undefined;
  readonly projects: readonly ProjectRecord[];
  /** Moves on when a task ends, so the counts are read again. */
  readonly revision: number;
  readonly onOpenProject: () => void;
  readonly canOpenProject: boolean;
}): React.JSX.Element {
  const [tasks, setTasks] = useState<readonly TaskRecord[] | undefined>();

  useLoad(listTasks, [revision], setTasks, () => {
    setTasks([]);
  });

  const greeting =
    name === undefined || name.trim() === "" ? "What's up next?" : `What's up next, ${name}?`;

  return (
    <section className="agent-home" aria-labelledby="agent-home-heading">
      <h1 id="agent-home-heading" className="agent-home-greeting">
        <Glyph icon="wind" hue="blue" size={40} />
        {greeting}
      </h1>
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
