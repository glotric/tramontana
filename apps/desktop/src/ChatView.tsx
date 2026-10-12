import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ConversationRecord,
  EventEnvelope,
  RoutingMode,
  TaskUpdate,
} from "@tramontana/shared";

import { isTerminal, projectState } from "./activity.js";
import { MODE_LABEL } from "./cards.js";
import type { ChatTurn } from "./chat.js";
import { attachmentsOf, outcomeOf, titleFrom, withEvent } from "./chat.js";
import { AttachButton, PendingFiles, SentFiles, useAttachments } from "./Attachments.js";
import { DictateButton, appendDictated, useDictation } from "./Dictation.js";
import { ChosenSkills, SlashMenu, usePersonalSkills, useSkillSlash } from "./SkillSlash.js";
import {
  cancelTask,
  createConversation,
  failureText,
  listTasks,
  replayTaskEvents,
  startTask,
  watchTask,
} from "./core.js";
import { Glyph } from "./Glyph.js";
import { foldFrame } from "./stream.js";
import type { LiveStream } from "./stream.js";

/** Where a chat's models may run, in the order the composer offers them. */
const MODES: readonly RoutingMode[] = ["localOnly", "automatic", "cloudPreferred"];

/** What each mode means for the next message. */
const MODE_TEXT: Record<RoutingMode, string> = {
  localOnly: "Local only: answered on this device, and nothing is sent to a cloud model.",
  automatic:
    "Automatic: a model is chosen for you, and anything sent to a cloud model is asked about first.",
  cloudPreferred:
    "Prefer cloud: a cloud model is chosen when one is available, and each release to it is asked about first.",
};

/**
 * Chat: questions and writing, with no project and no files (ADR 0057).
 *
 * Each message is a task in a projectless conversation, so it is routed,
 * disclosed, approved and recorded like any other. Its model is offered no
 * project tools, and it sees the earlier turns of the chat.
 */
export function ChatView({
  conversation,
  routingMode,
  onRoutingMode,
  onCreated,
  onTurnEnded,
  notice,
}: {
  readonly conversation: ConversationRecord | undefined;
  readonly routingMode: RoutingMode;
  readonly onRoutingMode: (mode: RoutingMode) => void;
  /** A chat was made for the first message sent from an empty one. */
  readonly onCreated: (created: ConversationRecord) => void;
  /** A turn finished, so the list of chats may have moved. */
  readonly onTurnEnded: () => void;
  /** A question put to the person, such as releasing the chat to a cloud model. */
  readonly notice?: React.ReactNode;
}): React.JSX.Element {
  const [turns, setTurns] = useState<readonly ChatTurn[]>([]);
  const [stream, setStream] = useState<{ taskId: string; live: LiveStream } | undefined>();
  const [draft, setDraft] = useState("");
  const [failure, setFailure] = useState<string | undefined>();
  const [sending, setSending] = useState(false);
  const files = useAttachments();
  // The person's own skills, chosen with `/` (ADR 0070). A chat has no
  // project, so only theirs are offered.
  const personalSkills = usePersonalSkills();
  const slash = useSkillSlash({
    draft,
    setDraft,
    personal: personalSkills.skills,
    onOpen: personalSkills.refresh,
  });
  const request = useRef(0);
  // The chat this window made for its own first message: loading it again
  // would race the turn already streaming into it.
  const made = useRef<string | undefined>(undefined);
  const input = useRef<HTMLTextAreaElement>(null);
  const dictation = useDictation((text) => {
    setDraft((current) => appendDictated(current, text));
    input.current?.focus();
  });
  const bottom = useRef<HTMLDivElement>(null);

  const conversationId = conversation?.conversationId;

  const onUpdate = useCallback(
    (update: TaskUpdate, taskId: string, current: number): void => {
      if (current !== request.current) return;
      if (update.kind === "event") {
        const { event } = update;
        setTurns((existing) => withEvent(existing, event));
        if (event.payload.kind === "messageCommitted") setStream(undefined);
        if (
          event.payload.kind === "taskCompleted" ||
          event.payload.kind === "taskFailed" ||
          event.payload.kind === "taskCancelled"
        ) {
          setStream(undefined);
          onTurnEnded();
        }
      } else {
        setStream((existing) => ({
          taskId,
          live: foldFrame(existing?.taskId === taskId ? existing.live : undefined, update.frame),
        }));
      }
    },
    [onTurnEnded],
  );

  useEffect(() => {
    // The chat this window just made already holds its first turn, and that
    // turn's updates must keep arriving.
    if (conversationId !== undefined && made.current === conversationId) return;
    made.current = undefined;
    const current = ++request.current;
    setFailure(undefined);
    setStream(undefined);
    setTurns([]);
    if (conversationId === undefined) return;
    // Stopped when the window turns to another chat, or goes.
    let stop: (() => void) | undefined;
    void (async () => {
      try {
        const records = [...(await listTasks(conversationId))].sort((a, b) =>
          a.createdAt === b.createdAt
            ? a.taskId.localeCompare(b.taskId)
            : a.createdAt.localeCompare(b.createdAt),
        );
        const loaded = await Promise.all(
          records.map(async (record) => ({
            taskId: record.taskId,
            goal: record.goalSummary,
            events: await replayTaskEvents(record.taskId),
          })),
        );
        if (current !== request.current) return;
        setTurns(loaded);
        // The newest turn may still be under way: follow it from where the
        // record ends.
        const latest = loaded.at(-1);
        const state = latest === undefined ? undefined : projectState(latest.events);
        const last = latest?.events.at(-1)?.sequence;
        if (
          latest !== undefined &&
          state !== undefined &&
          !isTerminal(state) &&
          last !== undefined
        ) {
          const unwatch = await watchTask(latest.taskId, last, (update) => {
            onUpdate(update, latest.taskId, current);
          });
          if (current === request.current) stop = unwatch;
          else unwatch();
        }
      } catch (error: unknown) {
        if (current === request.current)
          setFailure(failureText(error, "Could not load this chat."));
      }
    })();
    return () => {
      stop?.();
    };
  }, [conversationId, onUpdate]);

  useEffect(() => {
    bottom.current?.scrollIntoView({ block: "end" });
  }, [turns, stream]);

  const latest = turns.at(-1);
  const running = sending || (latest !== undefined && outcomeOf(latest.events).kind === "working");

  const send = useCallback(() => {
    const goal = draft.trim();
    if (goal === "" || running) return;
    setSending(true);
    setFailure(undefined);
    const tokens = files.tokens();
    const skills = slash.personalIds;
    void (async () => {
      try {
        let id = conversationId;
        if (id === undefined) {
          const created = await createConversation(undefined, titleFrom(goal));
          id = created.conversationId;
          made.current = id;
          onCreated(created);
        }
        const current = request.current;
        // Frames carry no task id; the task's first event, which comes
        // before any frame, says which task they are.
        let known: string | undefined;
        const taskId = await startTask(
          goal,
          (update) => {
            if (update.kind === "event") known = update.event.taskId;
            onUpdate(update, known ?? "", current);
          },
          id,
          {
            writes: false,
            recipes: false,
            apps: false,
            browse: false,
            routingMode,
            attachments: tokens,
            ...(skills.length === 0 ? {} : { personalSkills: skills }),
          },
        );
        setTurns((existing) =>
          existing.some((turn) => turn.taskId === taskId)
            ? existing
            : [...existing, { taskId, goal, events: [] as readonly EventEnvelope[] }],
        );
        setDraft("");
        files.sent(tokens);
        slash.clear();
      } catch (error: unknown) {
        setFailure(failureText(error, "Could not send the message."));
      } finally {
        setSending(false);
        input.current?.focus();
      }
    })();
  }, [conversationId, draft, files, onCreated, onUpdate, routingMode, running, slash]);

  const stop = useCallback(() => {
    if (latest === undefined) return;
    void cancelTask(latest.taskId).catch((error: unknown) => {
      setFailure(failureText(error, "Could not stop the answer."));
    });
  }, [latest]);

  const empty = turns.length === 0;

  return (
    // Empty, the composer sits under the greeting, as the design's home
    // does; once there is a conversation, it docks at the bottom.
    <div className="chat-view" data-empty={empty}>
      <div className="workspace-scroll">
        <div className="workspace-column chat">
          {failure !== undefined && (
            <div className="notice notice--error" role="alert">
              {failure}
            </div>
          )}
          {files.error !== undefined && (
            <div className="notice notice--error" role="alert">
              {files.error}
            </div>
          )}
          {dictation.error !== undefined && (
            <div className="notice notice--error" role="alert">
              {dictation.error}
            </div>
          )}
          {notice}
          {empty ? (
            <section className="chat-home" aria-labelledby="chat-home-heading">
              <Glyph icon="chat" hue="blue" size={60} />
              <h2 id="chat-home-heading">What can I help with?</h2>
              <p>
                Ask a question or write something. A chat has no access to your files or projects.
              </p>
            </section>
          ) : (
            <ol className="chat-turns" aria-label="Chat messages">
              {turns.map((turn) => {
                const outcome = outcomeOf(turn.events);
                const live = stream?.taskId === turn.taskId ? stream.live : undefined;
                return (
                  <li key={turn.taskId} className="chat-turn">
                    <SentFiles files={attachmentsOf(turn.events)} />
                    <p className="chat-bubble">{turn.goal}</p>
                    {outcome.kind === "answered" ? (
                      <div className="chat-answer">{outcome.text}</div>
                    ) : outcome.kind === "failed" ? (
                      <p className="chat-answer chat-answer--failed" role="status">
                        This answer failed. {outcome.message}
                      </p>
                    ) : outcome.kind === "stopped" ? (
                      <p className="chat-answer chat-answer--quiet">Stopped.</p>
                    ) : outcome.kind === "waiting" ? (
                      <p className="chat-answer chat-answer--quiet">
                        Paused. It can be resumed from Agent mode&apos;s task view.
                      </p>
                    ) : live !== undefined && live.text !== "" ? (
                      <div className="chat-answer" role="region" aria-label="Live answer">
                        {live.text}
                      </div>
                    ) : (
                      <p className="chat-answer chat-answer--quiet" role="status">
                        Thinking…
                      </p>
                    )}
                  </li>
                );
              })}
            </ol>
          )}
          <div ref={bottom} />
        </div>
      </div>

      <div className="composer-dock">
        <SlashMenu slash={slash} hasAny={personalSkills.skills.length > 0} />
        <form
          className="composer"
          onSubmit={(submitted) => {
            submitted.preventDefault();
            send();
          }}
        >
          <label className="sr-only" htmlFor="chat-message">
            Message
          </label>
          <PendingFiles files={files.files} onRemove={files.remove} />
          <ChosenSkills slash={slash} />
          <textarea
            ref={input}
            id="chat-message"
            value={draft}
            placeholder="Ask anything. Type / for your skills."
            rows={2}
            onChange={(changed) => {
              setDraft(changed.target.value);
            }}
            onKeyDown={(pressed) => {
              if (slash.onKeyDown(pressed)) {
                pressed.preventDefault();
                return;
              }
              if (
                pressed.key === "Enter" &&
                !pressed.shiftKey &&
                !pressed.nativeEvent.isComposing
              ) {
                pressed.preventDefault();
                send();
              }
            }}
          />
          <div className="composer-footer">
            <AttachButton onClick={files.choose} disabled={running} />
            <fieldset className="composer-modes" disabled={running}>
              <legend className="sr-only">Where models may run</legend>
              {MODES.map((mode) => (
                <label key={mode} data-selected={routingMode === mode}>
                  <input
                    type="radio"
                    name="chat-routing-mode"
                    value={mode}
                    checked={routingMode === mode}
                    onChange={() => {
                      onRoutingMode(mode);
                    }}
                  />
                  {MODE_LABEL[mode]}
                </label>
              ))}
            </fieldset>
            <span />
            <DictateButton
              state={dictation.state}
              seconds={dictation.seconds}
              onToggle={dictation.toggle}
            />
            {running && latest !== undefined && !sending ? (
              <button type="button" className="button button--stop" onClick={stop}>
                <span className="stop-square" aria-hidden="true" />
                Stop
              </button>
            ) : (
              <button
                type="submit"
                className="send-button"
                aria-label="Send"
                disabled={running || draft.trim() === ""}
              >
                <svg
                  width="18"
                  height="18"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden="true"
                >
                  <path d="M12 19V5M5 12l7-7 7 7" />
                </svg>
              </button>
            )}
          </div>
        </form>
        <p className="composer-disclosure">
          {MODE_TEXT[routingMode]}
          {slash.chosen.length === 0
            ? ""
            : slash.chosen.length === 1
              ? " It is given one skill to follow."
              : ` It is given ${slash.chosen.length} skills to follow.`}
        </p>
      </div>
    </div>
  );
}
