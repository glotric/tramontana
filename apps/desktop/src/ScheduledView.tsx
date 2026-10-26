import { useCallback, useEffect, useState } from "react";
import type { ProjectRecord, RoutingMode, Schedule, ScheduleView } from "@tramontana/shared";

import { MODE_LABEL } from "./cards.js";
import {
  errorMessage,
  createSchedule,
  deleteSchedule,
  listSchedules,
  runScheduleNow,
  setScheduleEnabled,
} from "./core.js";
import { Glyph } from "./Glyph.js";
import { DAY_NAME, WEEKDAYS, cadenceFrom, describeCadence, describeNext } from "./schedule.js";

const MODES: readonly RoutingMode[] = ["localOnly", "automatic", "cloudPreferred"];

/** Starting points that work with what Tramontana can actually do. */
const IDEAS: readonly {
  readonly title: string;
  readonly goal: string;
  readonly every: string;
  readonly time: string;
  readonly needsProject: boolean;
  readonly description: string;
}[] = [
  {
    title: "Morning plan",
    goal: "Suggest a short plan for today: three priorities, and one thing to leave for later.",
    every: "weekday",
    time: "08:00",
    needsProject: false,
    description: "Every weekday morning, a short plan with three priorities.",
  },
  {
    title: "Weekly project summary",
    goal: "Summarize what changed in this project this week, from its Git history.",
    every: "friday",
    time: "15:00",
    needsProject: true,
    description: "Every Friday, a summary of a project's week from its Git history.",
  },
];

interface Draft {
  readonly title: string;
  readonly goal: string;
  readonly where: string;
  readonly every: string;
  readonly time: string;
  readonly mode: RoutingMode;
}

const EMPTY: Draft = {
  title: "",
  goal: "",
  where: "",
  every: "weekday",
  time: "08:00",
  mode: "localOnly",
};

/**
 * The design's Scheduled screen (ADR 0063): regular work that runs while
 * Tramontana is open. Each run is a turn in the schedule's own conversation.
 */
export function ScheduledView({
  projects,
  onOpen,
}: {
  readonly projects: readonly ProjectRecord[];
  /** Show a schedule's conversation. */
  readonly onOpen: (schedule: Schedule) => void;
}): React.JSX.Element {
  const [schedules, setSchedules] = useState<readonly ScheduleView[] | undefined>();
  const [draft, setDraft] = useState<Draft | undefined>();
  const [failure, setFailure] = useState<string | undefined>();
  const [notice, setNotice] = useState<string | undefined>();
  const [deleting, setDeleting] = useState<string | undefined>();

  const refresh = useCallback(() => {
    void listSchedules().then(setSchedules, (error: unknown) => {
      setFailure(errorMessage(error, "Could not read the scheduled tasks."));
    });
  }, []);

  useEffect(() => {
    refresh();
    // What runs next moves on with the clock.
    const timer = setInterval(refresh, 60_000);
    return () => {
      clearInterval(timer);
    };
  }, [refresh]);

  const projectName = (projectId: string | undefined): string =>
    projectId === undefined
      ? "A chat"
      : (projects.find((project) => project.projectId === projectId)?.name ?? "A project");

  const cadence = draft === undefined ? undefined : cadenceFrom(draft.every, draft.time);

  return (
    <section className="projects-view" aria-labelledby="scheduled-heading">
      <header className="projects-header">
        <div className="projects-title">
          <Glyph icon="clock" hue="orange" size={64} />
          <div>
            <h1 id="scheduled-heading">Scheduled</h1>
            <p>Regular work Tramontana does for you while it is open.</p>
          </div>
        </div>
        <button
          type="button"
          className="button button--primary"
          onClick={() => {
            setDraft(EMPTY);
            setFailure(undefined);
          }}
        >
          New scheduled task
        </button>
      </header>

      <p className="notice" role="note">
        Nothing runs while Tramontana is closed. A run missed then happens once when you open it
        again. A run that needs your approval waits for you.
      </p>
      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}
      {notice !== undefined && (
        <p className="notice" role="status">
          {notice}
        </p>
      )}

      {draft !== undefined && (
        <form
          className="settings-card schedule-form"
          aria-label="New scheduled task"
          onSubmit={(submitted) => {
            submitted.preventDefault();
            if (cadence === undefined) return;
            setFailure(undefined);
            void createSchedule({
              title: draft.title,
              goal: draft.goal,
              cadence,
              routingMode: draft.mode,
              ...(draft.where === "" ? {} : { projectId: draft.where }),
            }).then(
              () => {
                setDraft(undefined);
                refresh();
              },
              (error: unknown) => {
                setFailure(errorMessage(error, "Could not create the scheduled task."));
              },
            );
          }}
        >
          <div className="settings-section">
            <label htmlFor="schedule-title">Name</label>
            <input
              id="schedule-title"
              value={draft.title}
              maxLength={120}
              onChange={(event) => {
                setDraft({ ...draft, title: event.target.value });
              }}
            />
            <label htmlFor="schedule-goal">What should it do?</label>
            <textarea
              id="schedule-goal"
              rows={3}
              value={draft.goal}
              maxLength={4000}
              onChange={(event) => {
                setDraft({ ...draft, goal: event.target.value });
              }}
            />
            <div className="schedule-when">
              <label>
                Where
                <select
                  value={draft.where}
                  onChange={(event) => {
                    setDraft({ ...draft, where: event.target.value });
                  }}
                >
                  <option value="">A chat (no files)</option>
                  {projects.map((project) => (
                    <option key={project.projectId} value={project.projectId}>
                      {project.name}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                When
                <select
                  value={draft.every}
                  onChange={(event) => {
                    setDraft({ ...draft, every: event.target.value });
                  }}
                >
                  <option value="day">Every day</option>
                  <option value="weekday">Every weekday</option>
                  {WEEKDAYS.map((day) => (
                    <option key={day} value={day}>
                      Every {DAY_NAME[day]}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                At
                <input
                  type="time"
                  value={draft.time}
                  onChange={(event) => {
                    setDraft({ ...draft, time: event.target.value });
                  }}
                />
              </label>
            </div>
            <fieldset className="composer-modes">
              <legend className="sr-only">Where models may run</legend>
              {MODES.map((mode) => (
                <label key={mode} data-selected={draft.mode === mode}>
                  <input
                    type="radio"
                    name="schedule-mode"
                    value={mode}
                    checked={draft.mode === mode}
                    onChange={() => {
                      setDraft({ ...draft, mode });
                    }}
                  />
                  {MODE_LABEL[mode]}
                </label>
              ))}
            </fieldset>
            <p className="settings-hint">
              Each run can read what it needs and changes nothing. Anything sent to a cloud model is
              asked about first.
            </p>
            <div className="menu-actions">
              <button
                type="button"
                className="button button--secondary"
                onClick={() => {
                  setDraft(undefined);
                }}
              >
                Cancel
              </button>
              <button
                type="submit"
                className="button button--primary"
                disabled={
                  cadence === undefined || draft.title.trim() === "" || draft.goal.trim() === ""
                }
              >
                Schedule
              </button>
            </div>
          </div>
        </form>
      )}

      <section aria-labelledby="coming-up">
        <h2 id="coming-up" className="section-title">
          Coming up
        </h2>
        {schedules === undefined ? (
          <p className="inline-empty">Reading the scheduled tasks…</p>
        ) : schedules.length === 0 ? (
          <p className="inline-empty">Nothing is scheduled yet.</p>
        ) : (
          <ul className="schedule-list" aria-label="Scheduled tasks">
            {schedules.map(({ schedule, nextRun }) => (
              <li key={schedule.scheduleId} className="schedule-row">
                <Glyph
                  icon={schedule.projectId === undefined ? "sun" : "chart"}
                  hue={schedule.projectId === undefined ? "yellow" : "blue"}
                  size={48}
                />
                <div className="schedule-text">
                  <strong>{schedule.title}</strong>
                  <span>
                    {describeCadence(schedule.cadence)} · {projectName(schedule.projectId)} ·{" "}
                    {nextRun === undefined
                      ? "Paused"
                      : `Next: ${describeNext(nextRun, new Date())}`}
                  </span>
                </div>
                <button
                  type="button"
                  className="button button--secondary"
                  onClick={() => {
                    onOpen(schedule);
                  }}
                >
                  Open
                </button>
                <button
                  type="button"
                  className="button button--secondary"
                  onClick={() => {
                    setNotice(undefined);
                    void runScheduleNow(schedule.scheduleId).then(
                      () => {
                        setNotice(`“${schedule.title}” is running now.`);
                        refresh();
                      },
                      (error: unknown) => {
                        setFailure(errorMessage(error, "Could not run it now."));
                      },
                    );
                  }}
                >
                  Run now
                </button>
                {deleting === schedule.scheduleId ? (
                  <span className="schedule-confirm" role="group" aria-label="Delete?">
                    <button
                      type="button"
                      className="button button--danger"
                      onClick={() => {
                        void deleteSchedule(schedule.scheduleId).then(
                          () => {
                            setDeleting(undefined);
                            setNotice(`Deleted “${schedule.title}”. Its conversation is kept.`);
                            refresh();
                          },
                          (error: unknown) => {
                            setFailure(errorMessage(error, "Could not delete it."));
                          },
                        );
                      }}
                    >
                      Delete
                    </button>
                    <button
                      type="button"
                      className="button button--secondary"
                      onClick={() => {
                        setDeleting(undefined);
                      }}
                    >
                      Keep
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="icon-button"
                    aria-label={`Delete ${schedule.title}`}
                    onClick={() => {
                      setDeleting(schedule.scheduleId);
                    }}
                  >
                    ×
                  </button>
                )}
                <input
                  type="checkbox"
                  role="switch"
                  className="switch"
                  aria-label={`${schedule.title} is ${schedule.enabled ? "on" : "paused"}`}
                  checked={schedule.enabled}
                  onChange={(event) => {
                    void setScheduleEnabled(schedule.scheduleId, event.target.checked).then(
                      refresh,
                      (error: unknown) => {
                        setFailure(errorMessage(error, "Could not change it."));
                      },
                    );
                  }}
                />
              </li>
            ))}
          </ul>
        )}
      </section>

      <section aria-labelledby="ideas">
        <h2 id="ideas" className="section-title">
          Ideas to get started
        </h2>
        <ul className="project-cards" aria-label="Ideas">
          {IDEAS.map((idea) => {
            const firstProject = projects[0];
            const unavailable = idea.needsProject && firstProject === undefined;
            return (
              <li key={idea.title}>
                <button
                  type="button"
                  className="project-card"
                  disabled={unavailable}
                  onClick={() => {
                    setFailure(undefined);
                    setDraft({
                      title: idea.title,
                      goal: idea.goal,
                      where: idea.needsProject ? (firstProject?.projectId ?? "") : "",
                      every: idea.every,
                      time: idea.time,
                      mode: "localOnly",
                    });
                  }}
                >
                  <Glyph
                    icon={idea.needsProject ? "chart" : "sun"}
                    hue={idea.needsProject ? "blue" : "yellow"}
                    size={52}
                  />
                  <strong>{idea.title}</strong>
                  <span className="project-card-meta">
                    {unavailable ? "Open a project first." : idea.description}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      </section>
    </section>
  );
}
