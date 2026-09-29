import { useCallback, useEffect, useRef, useState } from "react";

import type { DictationStatus } from "@tramontana/shared";

import {
  asErrorEnvelope,
  chooseDictation,
  clearDictation,
  dictationStatus,
  transcribe,
} from "./core.js";
import { MAX_SECONDS, microphoneProblem, startRecording } from "./recorder.js";
import type { Recording } from "./recorder.js";

export type DictationState = "idle" | "starting" | "recording" | "transcribing";

/** Put dictated words after what is already written. */
export function appendDictated(draft: string, text: string): string {
  if (draft.trim() === "") return text;
  return /\s$/.test(draft) ? `${draft}${text}` : `${draft} ${text}`;
}

/**
 * Speech to text for a composer (ADR 0065). Pressing once starts recording,
 * pressing again stops it; the recording is transcribed on this device and
 * the words handed to `onText`. Nothing is sent, and the audio is not kept.
 */
export function useDictation(onText: (text: string) => void): {
  readonly state: DictationState;
  readonly seconds: number;
  readonly error: string | undefined;
  readonly toggle: () => void;
} {
  const [state, setState] = useState<DictationState>("idle");
  const [seconds, setSeconds] = useState(0);
  const [error, setError] = useState<string | undefined>();
  const recording = useRef<Recording | undefined>(undefined);
  const deliver = useRef(onText);
  useEffect(() => {
    deliver.current = onText;
  }, [onText]);

  const finish = useCallback(() => {
    const current = recording.current;
    if (current === undefined) return;
    recording.current = undefined;
    setState("transcribing");
    void current
      .stop()
      .then(transcribe)
      .then(
        (text) => {
          deliver.current(text);
        },
        (failure: unknown) => {
          setError(asErrorEnvelope(failure)?.message ?? "The recording could not be transcribed.");
        },
      )
      .finally(() => {
        setState("idle");
      });
  }, []);

  const toggle = useCallback(() => {
    if (state === "recording") {
      finish();
      return;
    }
    if (state !== "idle") return;
    setError(undefined);
    setState("starting");
    void dictationStatus()
      .then(async (status) => {
        if (!status.configured) {
          setError(
            "Dictation is not set up. In Settings, choose a whisper.cpp program and a model; nothing is downloaded.",
          );
          setState("idle");
          return;
        }
        try {
          recording.current = await startRecording();
        } catch (failure: unknown) {
          setError(microphoneProblem(failure));
          setState("idle");
          return;
        }
        setSeconds(0);
        setState("recording");
      })
      .catch((failure: unknown) => {
        setError(asErrorEnvelope(failure)?.message ?? "Dictation is not available.");
        setState("idle");
      });
  }, [finish, state]);

  // The clock while recording, and the five-minute limit.
  useEffect(() => {
    if (state !== "recording") return undefined;
    const tick = window.setInterval(() => {
      setSeconds((current) => current + 1);
    }, 1000);
    return () => {
      window.clearInterval(tick);
    };
  }, [state]);

  useEffect(() => {
    if (state === "recording" && seconds >= MAX_SECONDS) finish();
  }, [finish, seconds, state]);

  // Leaving the screen stops the microphone and throws the recording away.
  useEffect(
    () => () => {
      recording.current?.cancel();
      recording.current = undefined;
    },
    [],
  );

  return { state, seconds, error, toggle };
}

function clock(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** The composer's microphone. */
export function DictateButton({
  state,
  seconds,
  onToggle,
  disabled,
}: {
  readonly state: DictationState;
  readonly seconds: number;
  readonly onToggle: () => void;
  readonly disabled?: boolean;
}): React.JSX.Element {
  const recording = state === "recording";
  const label = recording
    ? "Stop dictating"
    : state === "transcribing"
      ? "Transcribing"
      : "Dictate";
  return (
    <button
      type="button"
      className="dictate-button"
      data-state={state}
      aria-label={label}
      aria-pressed={recording}
      title={
        recording
          ? "Stop and turn the recording into text"
          : "Dictate, transcribed on this computer"
      }
      onClick={onToggle}
      disabled={disabled === true || state === "starting" || state === "transcribing"}
    >
      {recording ? (
        <>
          <span className="stop-square" aria-hidden="true" />
          <span className="dictate-clock">{clock(seconds)}</span>
        </>
      ) : state === "transcribing" ? (
        <span className="dictate-spinner" aria-hidden="true" />
      ) : (
        <svg
          width="18"
          height="18"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <rect x="9" y="3" width="6" height="11" rx="3" />
          <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
        </svg>
      )}
    </button>
  );
}

/**
 * Settings, Dictation (ADR 0065): the whisper.cpp program and model the
 * person chose. Tramontana downloads neither.
 */
export function DictationSettings(): React.JSX.Element {
  const [status, setStatus] = useState<DictationStatus | undefined>();
  const [language, setLanguage] = useState("");
  const [failure, setFailure] = useState<string | undefined>();

  useEffect(() => {
    let live = true;
    void dictationStatus().then(
      (loaded) => {
        if (!live) return;
        setStatus(loaded);
        setLanguage(loaded.language ?? "");
      },
      (error: unknown) => {
        if (live) setFailure(asErrorEnvelope(error)?.message ?? "Could not read the setup.");
      },
    );
    return () => {
      live = false;
    };
  }, []);

  const choose = (): void => {
    setFailure(undefined);
    const code = language.trim();
    void chooseDictation(code === "" ? undefined : code).then(
      (chosen) => {
        if (chosen !== undefined) setStatus(chosen);
      },
      (error: unknown) => {
        setFailure(asErrorEnvelope(error)?.message ?? "Could not set up dictation.");
      },
    );
  };

  return (
    <section className="settings-section" aria-labelledby="dictation-heading">
      <h2 id="dictation-heading">Dictation</h2>
      <p className="settings-hint">
        Speech is turned into text on this computer by a whisper.cpp you install. Nothing is
        downloaded, and recordings are not kept.
      </p>
      {failure !== undefined && (
        <div className="notice notice--error" role="alert">
          {failure}
        </div>
      )}
      {status?.configured === true ? (
        <dl className="dictation-setup">
          <dt>Program</dt>
          <dd>{status.binaryPath}</dd>
          <dt>Model</dt>
          <dd>{status.modelPath}</dd>
          <dt>Language</dt>
          <dd>{status.language ?? "Detected from speech"}</dd>
        </dl>
      ) : (
        <p className="inline-empty">
          {status === undefined ? "Reading the setup…" : "Not set up."}
        </p>
      )}
      <div className="dictation-language">
        <label htmlFor="dictation-language">Language code (optional, for example en or sl)</label>
        <input
          id="dictation-language"
          value={language}
          maxLength={8}
          placeholder="Detect from speech"
          onChange={(event) => {
            setLanguage(event.target.value);
          }}
        />
      </div>
      <div className="settings-actions">
        <button type="button" className="button" onClick={choose}>
          {status?.configured === true ? "Choose again…" : "Choose whisper-cli and a model…"}
        </button>
        {status?.configured === true && (
          <button
            type="button"
            className="button"
            onClick={() => {
              setFailure(undefined);
              void clearDictation().then(setStatus, (error: unknown) => {
                setFailure(asErrorEnvelope(error)?.message ?? "Could not remove the setup.");
              });
            }}
          >
            Remove
          </button>
        )}
      </div>
    </section>
  );
}
