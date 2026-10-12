import { useCallback, useEffect, useRef, useState } from "react";
import type { EventEnvelope, TaskUpdate } from "@tramontana/shared";

import { isTerminal, projectState } from "./activity.js";
import { failureText, replayTaskEvents, watchTask } from "./core.js";
import { foldFrame } from "./stream.js";
import type { LiveStream } from "./stream.js";

/** A task about to start, from the moment `begin` cleared the screen for it. */
export interface TaskBeginning {
  /** Delivers its updates, for as long as it is the task on screen. */
  readonly onUpdate: (update: TaskUpdate) => void;
  /**
   * Whether it still is: false once anything else was chosen. A start that
   * finishes after the person moved on must not take the screen back.
   */
  readonly isCurrent: () => boolean;
}

/** The task on screen: its record so far, and the answer streaming in. */
export interface TaskStream {
  readonly selectedTaskId: string | undefined;
  readonly events: readonly EventEnvelope[];
  readonly stream: LiveStream | undefined;
  /** Show a task's record, and follow it while it is still under way. */
  readonly select: (taskId: string) => Promise<void>;
  /** Clear the screen for a task about to start. */
  readonly begin: () => TaskBeginning;
  /** Name the task `begin` cleared the screen for, once its id is known. */
  readonly adopt: (taskId: string) => void;
  /** Show no task. */
  readonly clear: () => void;
}

/**
 * One stream of task updates on screen at a time.
 *
 * Every change of task goes through `next`, which drops whatever is still in
 * flight for the task before and stops following it: a window that turned
 * away used to leave the shell forwarding updates nobody would see until the
 * task ended. `onEvent` hears each new durable event; it is read from a ref,
 * so the handlers here stay the same functions for the life of the window.
 */
export function useTaskStream(
  onEvent: React.RefObject<(event: EventEnvelope) => void>,
  onFailure: (text: string | undefined) => void,
): TaskStream {
  const [selectedTaskId, setSelectedTaskId] = useState<string | undefined>();
  const [events, setEvents] = useState<readonly EventEnvelope[]>([]);
  const [stream, setStream] = useState<LiveStream | undefined>();
  const request = useRef(0);
  const seen = useRef(new Set<number>());
  const unwatch = useRef<(() => void) | undefined>(undefined);

  const next = useCallback((): number => {
    unwatch.current?.();
    unwatch.current = undefined;
    return ++request.current;
  }, []);

  // Stop following when the window goes.
  useEffect(
    () => () => {
      next();
    },
    [next],
  );

  const handleUpdate = useCallback(
    (update: TaskUpdate): void => {
      switch (update.kind) {
        case "event": {
          const { event } = update;
          if (seen.current.has(event.sequence)) return;
          seen.current.add(event.sequence);
          setEvents((previous) => [...previous, event]);
          // The turn's whole answer is now in the record, and its card
          // carries the text. Keeping the frames on screen as well would show
          // the same words twice and leave them there while the next turn ran.
          if (event.payload.kind === "messageCommitted") setStream(undefined);
          onEvent.current(event);
          break;
        }
        case "frame":
          setStream((current) => foldFrame(current, update.frame));
          break;
      }
    },
    [onEvent],
  );

  const select = useCallback(
    async (taskId: string): Promise<void> => {
      const current = next();
      setSelectedTaskId(taskId);
      setStream(undefined);
      onFailure(undefined);
      try {
        const replayed = await replayTaskEvents(taskId);
        if (current !== request.current) return;
        seen.current = new Set(replayed.map((event) => event.sequence));
        setEvents(replayed);
        // A task still under way goes on in the shell whether or not this
        // window was watching, as it was not across a reload. Follow it from
        // the last event the record holds, rather than show a record that
        // stopped when the window did.
        const state = projectState(replayed);
        const last = replayed.at(-1)?.sequence;
        if (
          state !== undefined &&
          !isTerminal(state) &&
          state !== "interrupted" &&
          last !== undefined
        ) {
          const stop = await watchTask(taskId, last, (update) => {
            if (current === request.current) handleUpdate(update);
          });
          // Another task may have been chosen while this one was attaching.
          if (current === request.current) unwatch.current = stop;
          else stop();
        }
      } catch (error: unknown) {
        if (current === request.current) {
          onFailure(failureText(error, "Could not replay the task history."));
        }
      }
    },
    [handleUpdate, next, onFailure],
  );

  const begin = useCallback(() => {
    const current = next();
    seen.current = new Set();
    setEvents([]);
    setStream(undefined);
    setSelectedTaskId(undefined);
    const isCurrent = (): boolean => current === request.current;
    return {
      onUpdate: (update: TaskUpdate): void => {
        if (isCurrent()) handleUpdate(update);
      },
      isCurrent,
    };
  }, [handleUpdate, next]);

  const clear = useCallback(() => {
    next();
    seen.current = new Set();
    setSelectedTaskId(undefined);
    setEvents([]);
    setStream(undefined);
  }, [next]);

  return { selectedTaskId, events, stream, select, begin, adopt: setSelectedTaskId, clear };
}
