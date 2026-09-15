import { MODE_LABEL } from "./cards.js";
import type { TaskHeader as TaskHeaderFacts } from "./cards.js";

/**
 * The task header: which worker, which model, where it ran, under which mode,
 * and in what state — each from the durable record, and each said as absent
 * rather than guessed when the record does not say yet.
 *
 * Retry does not re-run anything. It puts the goal back in the composer, where
 * the person chooses the mode and access again and starts a new task: a
 * failed task's choices are theirs to reconsider, not this header's to repeat.
 */

interface TaskHeaderProps {
  readonly header: TaskHeaderFacts;
  /** Put this goal back in the composer. Offered once a task has failed or been cancelled. */
  readonly onRetry: (goal: string) => void;
}

export function TaskHeader({ header, onRetry }: TaskHeaderProps): React.JSX.Element {
  const goal = header.goal;
  const retryable = header.state === "failed" || header.state === "cancelled";
  return (
    <section className="task-header" aria-label="Task summary">
      <dl className="task-facts">
        <div>
          <dt>State</dt>
          {/* Announced when it changes, so a task finishing is heard as
              well as seen. */}
          <dd data-state={header.state} aria-live="polite">
            {header.state ?? "not started"}
          </dd>
        </div>
        <div>
          <dt>Worker</dt>
          <dd>{header.worker ?? "not selected yet"}</dd>
        </div>
        <div>
          <dt>Model</dt>
          <dd>{header.model ?? "not routed yet"}</dd>
        </div>
        <div>
          <dt>Runs on</dt>
          <dd>{header.location ?? "not routed yet"}</dd>
        </div>
        <div>
          <dt>Mode</dt>
          <dd>{header.mode === undefined ? "not recorded" : MODE_LABEL[header.mode]}</dd>
        </div>
        <div>
          <dt>Data</dt>
          <dd data-left={header.leftDevice}>
            {header.leftDevice ? "Released off this device" : "Nothing released off this device"}
          </dd>
        </div>
      </dl>
      {retryable && goal !== undefined && (
        <button
          type="button"
          className="button button--secondary"
          onClick={() => {
            onRetry(goal);
          }}
        >
          Retry…
        </button>
      )}
    </section>
  );
}
