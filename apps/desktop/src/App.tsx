import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isSchemaCompatible, SCHEMA_VERSION } from "@tramontana/shared";
import type { EventEnvelope } from "@tramontana/shared";

import { describeEvent, isTerminal, projectState } from "./activity.js";
import { asErrorEnvelope, cancelTask, fetchCoreInfo, startTask } from "./core.js";

/** Result of the startup handshake with the Rust core. */
type CoreStatus =
  | { readonly kind: "connecting" }
  | { readonly kind: "ready"; readonly appVersion: string }
  | { readonly kind: "incompatible"; readonly reported: number }
  | { readonly kind: "unavailable" };

function describeCoreStatus(status: CoreStatus): string {
  switch (status.kind) {
    case "connecting":
      return "Connecting to core…";
    case "ready":
      return `Core ${status.appVersion} · contracts v${SCHEMA_VERSION}`;
    case "incompatible":
      return `Core speaks contracts v${status.reported}; this build speaks v${SCHEMA_VERSION}`;
    case "unavailable":
      return "Core unavailable — interface running outside the desktop shell";
  }
}

/**
 * The M1 walking skeleton.
 *
 * Starts a task through the same application service the CLI calls, renders its
 * ordered activity, and cancels it. Deliberately plain: the work surface is M8,
 * and building it now would mean building it against contracts that have not
 * been proven yet.
 */
export function App(): React.JSX.Element {
  const [status, setStatus] = useState<CoreStatus>({ kind: "connecting" });
  const [goal, setGoal] = useState("Say hello");
  const [taskId, setTaskId] = useState<string | undefined>();
  const [events, setEvents] = useState<readonly EventEnvelope[]>([]);
  const [streamed, setStreamed] = useState("");
  const [failure, setFailure] = useState<string | undefined>();

  // Sequences already rendered. A subscription starts before replay, so an
  // event can arrive twice; de-duplicating is cheaper than risking a gap.
  const seen = useRef(new Set<number>());

  useEffect(() => {
    let cancelled = false;
    void fetchCoreInfo().then(
      (info) => {
        if (cancelled) return;
        setStatus(
          isSchemaCompatible(info.schemaVersion)
            ? { kind: "ready", appVersion: info.appVersion }
            : { kind: "incompatible", reported: info.schemaVersion },
        );
      },
      () => {
        if (!cancelled) setStatus({ kind: "unavailable" });
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const state = useMemo(() => projectState(events), [events]);
  const running = taskId !== undefined && state !== undefined && !isTerminal(state);

  const onStart = useCallback(() => {
    seen.current = new Set();
    setEvents([]);
    setStreamed("");
    setFailure(undefined);
    setTaskId(undefined);

    void startTask(goal, (update) => {
      // A switch rather than if/else: `TaskUpdate` is non-exhaustive in Rust,
      // so a newer core may deliver a kind this build has never seen. Falling
      // through is correct — the durable events still arrive as `event`.
      switch (update.kind) {
        case "event": {
          const { event } = update;
          if (seen.current.has(event.sequence)) return;
          seen.current.add(event.sequence);
          setEvents((previous) => [...previous, event]);
          break;
        }
        case "frame": {
          setStreamed((previous) => previous + update.frame.text);
          break;
        }
      }
    }).then(
      (id) => {
        setTaskId(id);
      },
      (error: unknown) => {
        const envelope = asErrorEnvelope(error);
        setFailure(
          envelope ? `${envelope.code}: ${envelope.message}` : "Could not start the task.",
        );
      },
    );
  }, [goal]);

  const onCancel = useCallback(() => {
    if (taskId === undefined) return;
    void cancelTask(taskId).catch((error: unknown) => {
      const envelope = asErrorEnvelope(error);
      setFailure(envelope ? `${envelope.code}: ${envelope.message}` : "Could not cancel the task.");
    });
  }, [taskId]);

  return (
    <main className="shell">
      <header className="shell__header">
        <h1 className="shell__brand">Tramontana</h1>
        <p className="shell__tagline">
          Milestone M1 — walking skeleton. One task through the real service, event, and IPC
          boundaries, driven by fakes.
        </p>
      </header>

      <form
        className="shell__controls"
        onSubmit={(submitted) => {
          submitted.preventDefault();
          onStart();
        }}
      >
        <label className="shell__label" htmlFor="goal">
          Goal
        </label>
        <input
          id="goal"
          className="shell__input"
          value={goal}
          onChange={(changed) => {
            setGoal(changed.target.value);
          }}
          disabled={running}
        />
        <button type="submit" className="shell__button" disabled={running || goal.trim() === ""}>
          Start task
        </button>
        <button
          type="button"
          className="shell__button shell__button--secondary"
          onClick={onCancel}
          disabled={!running}
        >
          Stop
        </button>
      </form>

      {failure !== undefined && (
        <p className="shell__failure" role="alert">
          {failure}
        </p>
      )}

      <section aria-labelledby="activity-heading" className="shell__activity">
        <h2 id="activity-heading" className="shell__section-title">
          Activity
        </h2>
        {events.length === 0 ? (
          <p className="shell__empty">No task yet. Start one to see its ordered event stream.</p>
        ) : (
          <ol className="shell__events">
            {events.map((event) => {
              const line = describeEvent(event);
              return (
                <li key={event.eventId} className="shell__event">
                  <span className="shell__event-sequence">{line.sequence}</span>
                  <span className="shell__event-kind">{line.kind}</span>
                  <span className="shell__event-detail">{line.detail}</span>
                </li>
              );
            })}
          </ol>
        )}
        {streamed !== "" && (
          <p className="shell__stream">
            <span className="shell__stream-label">streaming</span>
            {streamed}
          </p>
        )}
      </section>

      <footer className="shell__status">
        <span className="shell__badge" data-state={status.kind}>
          {describeCoreStatus(status)}
        </span>
        {state !== undefined && (
          <span className="shell__badge" data-task-state={state}>
            {state}
          </span>
        )}
      </footer>
    </main>
  );
}
