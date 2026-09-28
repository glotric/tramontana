import { useEffect, useMemo, useState } from "react";
import type { ProjectRecord } from "@tramontana/shared";

import { listConversations } from "./core.js";
import { Glyph } from "./Glyph.js";
import type { GlyphHue, GlyphIcon } from "./Glyph.js";

/** The icons a project card is drawn with, chosen by its id so it never moves. */
const LOOKS: readonly (readonly [GlyphIcon, GlyphHue])[] = [
  ["doc", "blue"],
  ["wrench", "orange"],
  ["book", "green"],
  ["chat", "purple"],
  ["board", "red"],
  ["folder", "yellow"],
  ["chart", "teal"],
];

function lookOf(projectId: string): readonly [GlyphIcon, GlyphHue] {
  let sum = 0;
  for (const character of projectId) sum = (sum * 31 + character.charCodeAt(0)) >>> 0;
  return LOOKS[sum % LOOKS.length] ?? ["folder", "yellow"];
}

/** A date the way a person reads it. */
function describeDay(timestamp: string): string {
  const day = new Date(timestamp);
  if (Number.isNaN(day.getTime())) return "";
  return day.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

/**
 * The design's Projects gallery: every project in the profile as a card, with
 * what the record knows about it: its name, its folder, how many
 * conversations it holds and when it was opened. It shows nothing the record
 * does not hold.
 */
export function ProjectsView({
  projects,
  onOpen,
  onNew,
  opening,
}: {
  readonly projects: readonly ProjectRecord[];
  readonly onOpen: (project: ProjectRecord) => void;
  readonly onNew: () => void;
  readonly opening: boolean;
}): React.JSX.Element {
  const [filter, setFilter] = useState("");
  const [counts, setCounts] = useState<Readonly<Record<string, number>>>({});

  useEffect(() => {
    let active = true;
    void Promise.all(
      projects.map(async (project) => {
        const conversations = await listConversations(project.projectId).catch(() => undefined);
        return [project.projectId, conversations?.length] as const;
      }),
    ).then((found) => {
      if (!active) return;
      const known: Record<string, number> = {};
      for (const [projectId, count] of found) {
        if (count !== undefined) known[projectId] = count;
      }
      setCounts(known);
    });
    return () => {
      active = false;
    };
  }, [projects]);

  const shown = useMemo(() => {
    const wanted = filter.trim().toLowerCase();
    if (wanted === "") return projects;
    return projects.filter(
      (project) =>
        project.name.toLowerCase().includes(wanted) ||
        project.roots.some((root) => root.toLowerCase().includes(wanted)),
    );
  }, [filter, projects]);

  return (
    <section className="projects-view" aria-labelledby="projects-heading">
      <header className="projects-header">
        <div className="projects-title">
          <Glyph icon="folder" hue="yellow" size={64} />
          <div>
            <h1 id="projects-heading">Projects</h1>
            <p>A project is a folder on this computer that Agent tasks can work in.</p>
          </div>
        </div>
        <div className="projects-actions">
          <label className="projects-filter">
            <span className="sr-only">Filter projects</span>
            <input
              type="search"
              value={filter}
              placeholder="Filter projects"
              onChange={(changed) => {
                setFilter(changed.target.value);
              }}
            />
          </label>
          <button
            type="button"
            className="button button--primary"
            onClick={onNew}
            disabled={opening}
          >
            {opening ? "Opening…" : "Open a folder"}
          </button>
        </div>
      </header>

      <ul className="project-cards" aria-label="Projects">
        {shown.map((project) => {
          const [icon, hue] = lookOf(project.projectId);
          const count = counts[project.projectId];
          return (
            <li key={project.projectId}>
              <button
                type="button"
                className="project-card"
                onClick={() => {
                  onOpen(project);
                }}
              >
                <Glyph icon={icon} hue={hue} size={60} />
                <strong>{project.name}</strong>
                <span className="project-card-root">{project.roots[0]}</span>
                <span className="project-card-meta">
                  {count === undefined
                    ? ""
                    : `${count} ${count === 1 ? "conversation" : "conversations"} · `}
                  Opened {describeDay(project.createdAt)}
                </span>
              </button>
            </li>
          );
        })}
        {filter.trim() === "" && (
          <li>
            <button
              type="button"
              className="project-card project-card--new"
              onClick={onNew}
              disabled={opening}
            >
              <Glyph icon="plus" hue="teal" size={52} />
              Open a folder as a project
            </button>
          </li>
        )}
      </ul>
      {shown.length === 0 && filter.trim() !== "" && (
        <p className="inline-empty">No project matches “{filter.trim()}”.</p>
      )}
    </section>
  );
}
