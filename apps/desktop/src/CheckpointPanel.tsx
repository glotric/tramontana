import { useCallback, useEffect, useState } from "react";
import type { PendingAction, ReplaySafety, TaskCheckpoint } from "@tramontana/shared";

import {
  asErrorEnvelope,
  discardCheckpoint,
  handOffTask,
  handoffOptions,
  latestCheckpoint,
  resumeTask,
  type WorkerView,
} from "./core.js";

/**
 * The checkpoint inspector.
 *
 * `BUILD_ORDER.md` §12 asks for a surface showing the pending action, its
 * replay safety, and a resume-or-discard decision. The order matters: this
 * shows what happened *before* it offers either button, because the whole
 * point of an interrupted task is that the person, not the application,
 * decides whether repeating the last action is acceptable.
 *
 * Resume sends the identifier of the checkpoint that is on screen. Asking the
 * core to resume "the latest" would let this component act on a checkpoint
 * nobody has read.
 */

/** How a pending action's replay safety reads to a person. */
const SAFETY_TEXT: Record<ReplaySafety, string> = {
  safe: "Repeating this changes nothing outside Tramontana.",
  unsafe: "Repeating this could duplicate an effect.",
  unknown: "The record does not say whether repeating this is safe.",
};

/** What kind of work was in flight. */
const KIND_TEXT: Record<PendingAction["kind"], string> = {
  model: "A model request",
  tool: "A tool call",
  approval: "An approval",
  worker: "A worker session",
};

interface CheckpointPanelProps {
  /** The interrupted task. */
  readonly taskId: string;
  /**
   * Why the worker stopped, when it stopped because it could not go on
   * rather than because someone paused it (ADR 0048).
   */
  readonly stoppedBecause?: string | undefined;
  /** Called after resume or discard changes the task, so the shell reloads. */
  readonly onChanged: () => void;
}

export function CheckpointPanel({
  taskId,
  stoppedBecause,
  onChanged,
}: CheckpointPanelProps): React.JSX.Element {
  const [checkpoint, setCheckpoint] = useState<TaskCheckpoint | undefined>();
  const [workers, setWorkers] = useState<readonly WorkerView[]>([]);
  const [escalating, setEscalating] = useState(false);
  const [receiving, setReceiving] = useState<string>("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | undefined>();

  // No synchronous reset here: the shell keys this panel by task id, so a
  // different task is a different instance that starts in its own loading
  // state. Resetting inside the effect would re-render for no new information.
  useEffect(() => {
    let active = true;
    void latestCheckpoint(taskId).then(
      (found) => {
        if (!active) return;
        setCheckpoint(found);
        setLoading(false);
      },
      (error: unknown) => {
        if (!active) return;
        setFailure(asErrorEnvelope(error)?.message ?? "Could not read the checkpoint.");
        setLoading(false);
      },
    );
    return () => {
      active = false;
    };
  }, [taskId]);

  // The workers it could go to, in the core's order: the one that already
  // has the task is left out, a Local Only task is offered nothing off this
  // device, and a task whose model ran out is offered the cloud first
  // (ADR 0055). A failure to list them leaves only resume.
  useEffect(() => {
    let active = true;
    void handoffOptions(taskId).then(
      (offer) => {
        if (!active) return;
        setWorkers(offer.workers);
        setEscalating(offer.escalating);
      },
      () => undefined,
    );
    return () => {
      active = false;
    };
  }, [taskId]);

  const act = useCallback(
    async (action: () => Promise<void>, fallback: string): Promise<void> => {
      setBusy(true);
      setFailure(undefined);
      try {
        await action();
        onChanged();
      } catch (error: unknown) {
        // The core's refusal is the useful message: it names the rule, such as
        // a stale checkpoint or a changed worker.
        setFailure(asErrorEnvelope(error)?.message ?? fallback);
      } finally {
        setBusy(false);
      }
    },
    [onChanged],
  );

  if (loading) {
    return (
      <section className="checkpoint" aria-labelledby="checkpoint-heading">
        <h2 id="checkpoint-heading">Interrupted</h2>
        <p className="inline-empty">Reading the checkpoint…</p>
      </section>
    );
  }

  if (checkpoint === undefined) {
    return (
      <section className="checkpoint" aria-labelledby="checkpoint-heading">
        <h2 id="checkpoint-heading">Interrupted</h2>
        <p className="inline-empty">
          This task stopped without a checkpoint, so there is nothing to resume from.
        </p>
        {failure !== undefined && (
          <p className="notice notice--error" role="alert">
            {failure}
          </p>
        )}
      </section>
    );
  }

  const pending = checkpoint.pendingAction;
  const replayable = pending === undefined || pending.replaySafety === "safe";
  const resumable = checkpoint.resume.eligible && replayable;
  // Already ordered, and already filtered: which model a stopped task should
  // try next is a decision, and it is the core's (ADR 0055).
  const others = workers;
  const chosen = others.find((worker) => worker.agentId === receiving) ?? others[0];

  return (
    <section className="checkpoint" aria-labelledby="checkpoint-heading">
      <div className="section-heading-row">
        <h2 id="checkpoint-heading">
          {stoppedBecause === undefined ? "Interrupted" : "Stopped: the worker could not go on"}
        </h2>
        <code>{checkpoint.checkpointId}</code>
      </div>

      {stoppedBecause !== undefined && (
        <p className="notice" role="note">
          {stoppedBecause}{" "}
          {chosen === undefined
            ? "Configure another model to hand it to, or resume it once this one can go on."
            : `Hand it to ${chosen.displayName} to continue from here, with a budget of its own.`}
        </p>
      )}

      {/* Why the cloud is being offered first, and what it would cost in
          privacy. Said before the button, not after (ADR 0055). */}
      {escalating && chosen?.cloud === true && (
        <p className="notice" role="note">
          This one ran out of what its model could do, so a model off this device is offered first.
          Handing it to {chosen.displayName} sends the task&rsquo;s record (its goal, what it read
          and what it said) to that provider. Each release is still shown and approved as it
          happens, and nothing moves until you click.
        </p>
      )}

      <dl className="checkpoint-facts">
        <dt>Stopped at</dt>
        <dd>event {checkpoint.eventSequence}</dd>

        <dt>Completed steps</dt>
        <dd>
          {checkpoint.completedStepIds.length === 0
            ? "None. Nothing has been done yet."
            : `${checkpoint.completedStepIds.length} (a resume will not repeat them)`}
        </dd>

        <dt>In flight</dt>
        <dd>
          {pending === undefined ? (
            "Nothing. The task stopped cleanly between steps."
          ) : (
            <>
              <span data-replay-safety={pending.replaySafety}>
                {KIND_TEXT[pending.kind]}: {pending.displaySummary}
              </span>
              <br />
              {SAFETY_TEXT[pending.replaySafety]}
            </>
          )}
        </dd>
      </dl>

      {!resumable && (
        <p className="notice" role="note">
          {checkpoint.resume.eligible
            ? "This cannot be resumed automatically because the last action may have already had an effect. Check what happened before deciding."
            : `This checkpoint is not resumable (${checkpoint.resume.reasonCode ?? "no reason recorded"}).`}
        </p>
      )}

      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}

      <div className="checkpoint-actions">
        <button
          type="button"
          className="button button--primary"
          disabled={!resumable || busy}
          onClick={() =>
            void act(
              () => resumeTask(taskId, checkpoint.checkpointId),
              "Could not resume this checkpoint.",
            )
          }
        >
          Resume
        </button>
        <button
          type="button"
          className="button button--secondary"
          disabled={busy}
          onClick={() => void act(() => discardCheckpoint(taskId), "Could not discard this work.")}
        >
          Discard
        </button>
      </div>

      {others.length > 0 && chosen !== undefined && (
        <div className="checkpoint-handoff">
          <label>
            Hand over to
            <select
              value={chosen.agentId}
              disabled={busy}
              onChange={(changed) => {
                setReceiving(changed.target.value);
              }}
            >
              {others.map((worker) => (
                <option key={worker.agentId} value={worker.agentId}>
                  {worker.displayName}
                  {worker.cloud === true ? " (off this device)" : ""}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="button button--primary"
            disabled={!resumable || busy}
            onClick={() =>
              void act(
                () => handOffTask(taskId, checkpoint.checkpointId, chosen.agentId),
                "Could not hand this task over.",
              )
            }
          >
            Hand over
          </button>
          <p className="checkpoint-note">
            The worker continues from the task's record. What it may do stays what this task was
            allowed, and each approval and cloud release is asked again.
          </p>
        </div>
      )}
      <p className="checkpoint-note">
        Discarding keeps the history and undoes nothing that already happened.
      </p>
    </section>
  );
}
