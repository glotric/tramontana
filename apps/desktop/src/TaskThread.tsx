import type { EventEnvelope, TaskRecord } from "@tramontana/shared";

import { describeEvent } from "./activity.js";
import { ActivityCards } from "./ActivityCards.js";
import type { ActivityCard, TaskHeader as TaskHeaderFacts } from "./cards.js";
import { TaskHeader } from "./TaskHeader.js";
import type { LiveStream } from "./stream.js";

const OUTCOMES = new Set<ActivityCard["kind"]>(["completed", "cancelled", "failed"]);

/**
 * Split a task's cards into the steps it worked through and what it came
 * to: its last answer and how it ended. The steps fold away; the answer and
 * the outcome never do.
 */
export function splitCards(cards: readonly ActivityCard[]): {
  readonly steps: readonly ActivityCard[];
  readonly result: readonly ActivityCard[];
} {
  let answer = -1;
  cards.forEach((card, index) => {
    if (card.kind === "message") answer = index;
  });
  const steps: ActivityCard[] = [];
  const result: ActivityCard[] = [];
  cards.forEach((card, index) => {
    if (index === answer || OUTCOMES.has(card.kind)) result.push(card);
    else steps.push(card);
  });
  return { steps, result };
}

function describeTime(at: string): string {
  const date = new Date(at);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleString([], {
        day: "numeric",
        month: "short",
        hour: "numeric",
        minute: "2-digit",
      });
}

/**
 * A conversation's tasks as a thread, oldest first. The task on screen is
 * open: what was asked, its summary, the steps (folded unless it is still
 * running), and what it came to. The others are one line each.
 */
export function TaskThread({
  tasks,
  selectedTaskId,
  onSelectTask,
  header,
  cards,
  events,
  stream,
  running,
  onRetry,
  interruption,
}: {
  /** Newest first, as the record lists them. */
  readonly tasks: readonly TaskRecord[];
  readonly selectedTaskId: string | undefined;
  readonly onSelectTask: (taskId: string) => void;
  readonly header: TaskHeaderFacts;
  readonly cards: readonly ActivityCard[];
  /** The record itself, listed one line per event as the CLI replays it. */
  readonly events: readonly EventEnvelope[];
  readonly stream: LiveStream | undefined;
  readonly running: boolean;
  readonly onRetry: (goal: string) => void;
  /** What to show for an interrupted task: its checkpoint. */
  readonly interruption: React.ReactNode;
}): React.JSX.Element {
  const ordered = [...tasks].reverse();
  const { steps, result } = splitCards(cards);
  // A task started a moment ago is on screen before its record is listed.
  const unlisted = selectedTaskId !== undefined && !tasks.some((t) => t.taskId === selectedTaskId);

  const open = (
    <div className="thread-task">
      <TaskHeader header={header} onRetry={onRetry} />
      {interruption}
      {steps.length > 0 && (
        <details className="thread-steps" open={running}>
          <summary>
            {running
              ? `Working · ${steps.length} ${steps.length === 1 ? "step" : "steps"} so far`
              : `Worked through ${steps.length} ${steps.length === 1 ? "step" : "steps"}`}
          </summary>
          <ActivityCards cards={steps} />
        </details>
      )}
      {stream !== undefined && stream.text !== "" && (
        <div className="stream-output" role="region" aria-label="Live answer">
          {/* A window that joined an answer already under way holds only the
              end of it, and says so rather than letting a fragment read as
              the whole thing. */}
          {stream.partial && <span>joined part-way</span>}
          <p>{stream.text}</p>
        </div>
      )}
      {result.length > 0 && <ActivityCards cards={result} />}
      {steps.length === 0 && result.length === 0 && stream === undefined && (
        <ActivityCards cards={[]} />
      )}
      {events.length > 0 && (
        <details className="raw-events">
          <summary>All events ({events.length})</summary>
          <ol className="event-list" aria-label="Task event stream">
            {events.map((event) => {
              const line = describeEvent(event);
              return (
                <li key={event.eventId}>
                  <span className="event-sequence">{line.sequence}</span>
                  <code>{line.kind}</code>
                  <span>{line.detail}</span>
                </li>
              );
            })}
          </ol>
        </details>
      )}
    </div>
  );

  return (
    <ol className="thread" aria-label="Tasks in this conversation">
      {ordered.map((task) => {
        const selected = task.taskId === selectedTaskId;
        return (
          <li key={task.taskId} className="thread-item" data-selected={selected}>
            <button
              type="button"
              className="thread-goal"
              aria-current={selected ? "true" : undefined}
              aria-expanded={selected}
              onClick={() => {
                if (!selected) onSelectTask(task.taskId);
              }}
            >
              <span className="thread-goal-text">{task.goalSummary}</span>
              <span className="thread-goal-meta">
                <span data-task-state={task.state}>{task.state}</span>
                <span>{describeTime(task.createdAt)}</span>
              </span>
            </button>
            {selected && open}
          </li>
        );
      })}
      {unlisted && (
        <li className="thread-item" data-selected="true">
          {open}
        </li>
      )}
    </ol>
  );
}
