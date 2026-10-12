import { useEffect, useMemo, useState } from "react";
import type { ProjectRecord, TaskRecord } from "@tramontana/shared";

import { heatmap, intensity, perProject, summarize, withinRange } from "./activity-overview.js";
import type { ActivityRange } from "./activity-overview.js";

const RANGES: readonly (readonly [ActivityRange, string])[] = [
  ["all", "All"],
  ["30d", "30d"],
  ["7d", "7d"],
];

/** An hour of the day the way this computer writes it, such as "12 PM". */
function describeHour(hour: number): string {
  return new Date(2000, 0, 1, hour).toLocaleTimeString([], { hour: "numeric" });
}

/**
 * The current moment, moved on at each local midnight. It holds still
 * between, so the counts do not shift while the card is read, yet a window
 * left open overnight shows the new day rather than the one it opened on.
 */
function useToday(): Date {
  const [today, setToday] = useState(() => new Date());
  useEffect(() => {
    const midnight = new Date(today.getFullYear(), today.getMonth(), today.getDate() + 1);
    const timer = setTimeout(
      () => {
        setToday(new Date());
      },
      Math.max(1000, midnight.getTime() - Date.now()),
    );
    return () => {
      clearTimeout(timer);
    };
  }, [today]);
  return today;
}

function describeDay(date: Date): string {
  return date.toLocaleDateString([], { day: "numeric", month: "long" });
}

/**
 * The Agent home's activity card: what this profile's tasks add up to.
 * Counted from the records, never estimated; a profile with no tasks says
 * so rather than showing a grid of zeros.
 */
export function ActivityOverview({
  tasks,
  projects,
  now,
}: {
  /** `undefined` while the records are being read. */
  readonly tasks: readonly TaskRecord[] | undefined;
  readonly projects: readonly ProjectRecord[];
  readonly now?: Date;
}): React.JSX.Element {
  const [tab, setTab] = useState<"overview" | "projects">("overview");
  const [range, setRange] = useState<ActivityRange>("all");
  const current = useToday();
  const today = now ?? current;

  const chosen = useMemo(() => withinRange(tasks ?? [], range, today), [tasks, range, today]);
  const summary = useMemo(() => summarize(chosen), [chosen]);
  const byProject = useMemo(() => perProject(chosen, projects), [chosen, projects]);
  const weeks = useMemo(() => heatmap(tasks ?? [], today), [tasks, today]);
  const busiest = useMemo(
    () => Math.max(0, ...weeks.flat().map((day) => day?.count ?? 0)),
    [weeks],
  );

  const tiles: readonly (readonly [string, string])[] = [
    ["Tasks", String(summary.tasks)],
    ["Completed", String(summary.completed)],
    ["Stopped or failed", String(summary.stopped)],
    ["Active days", String(summary.activeDays)],
    ["Busiest hour", summary.busiestHour === undefined ? "—" : describeHour(summary.busiestHour)],
    ["Projects with tasks", String(summary.projects)],
  ];

  return (
    <section className="overview" aria-label="Your activity">
      <div className="overview-bar">
        <div className="overview-segments" role="group" aria-label="Show">
          <button
            type="button"
            aria-pressed={tab === "overview"}
            onClick={() => {
              setTab("overview");
            }}
          >
            Overview
          </button>
          <button
            type="button"
            aria-pressed={tab === "projects"}
            onClick={() => {
              setTab("projects");
            }}
          >
            Projects
          </button>
        </div>
        <div className="overview-segments" role="group" aria-label="Period">
          {RANGES.map(([value, label]) => (
            <button
              key={value}
              type="button"
              aria-pressed={range === value}
              onClick={() => {
                setRange(value);
              }}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {tasks === undefined ? (
        <p className="overview-empty">Reading your tasks…</p>
      ) : tasks.length === 0 ? (
        <p className="overview-empty">No tasks yet. Your activity will appear here.</p>
      ) : tab === "overview" ? (
        <>
          <dl className="overview-tiles">
            {tiles.map(([label, value]) => (
              <div key={label}>
                <dt>{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
          <div
            className="overview-heatmap"
            role="img"
            aria-label={`Tasks per day over the last ${weeks.length} weeks. The busiest day had ${busiest}.`}
          >
            {weeks.map((week, column) => (
              // A week has no identity beyond its place in the grid.
              <div key={`week-${column}`} className="overview-week">
                {week.map((day, row) =>
                  day === undefined ? (
                    <span key={`future-${row}`} className="overview-day" data-level="future" />
                  ) : (
                    <span
                      key={day.key}
                      className="overview-day"
                      data-level={intensity(day.count, busiest)}
                      title={`${day.count === 1 ? "1 task" : `${day.count} tasks`} on ${describeDay(day.date)}`}
                    />
                  ),
                )}
              </div>
            ))}
          </div>
        </>
      ) : byProject.length === 0 ? (
        <p className="overview-empty">No project tasks in this period.</p>
      ) : (
        <ol className="overview-projects" aria-label="Tasks per project">
          {byProject.map((entry) => (
            <li key={entry.projectId}>
              <span>{entry.name}</span>
              <span
                className="overview-bar-fill"
                style={{ width: `${(entry.tasks / (byProject[0]?.tasks ?? 1)) * 100}%` }}
                aria-hidden="true"
              />
              <strong>{entry.tasks}</strong>
            </li>
          ))}
        </ol>
      )}
      <p className="overview-note">Counted from the task records in this profile.</p>
    </section>
  );
}
