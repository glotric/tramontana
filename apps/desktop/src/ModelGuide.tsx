import { useCallback, useEffect, useState } from "react";

import { modelSetupHints } from "./core.js";
import type { ModelSetupHints } from "./core.js";
import { Icon } from "./icons.js";

/** Models that call tools well enough for Agent tasks, smallest first. */
const MODELS = [
  {
    name: "Qwen 2.5 3B Instruct (Q4_K_M)",
    file: "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
    url: "https://huggingface.co/bartowski/Qwen2.5-3B-Instruct-GGUF/resolve/main/Qwen2.5-3B-Instruct-Q4_K_M.gguf",
    size: "about 2 GB",
    fits: "for a computer with 8 GB of memory; quick, weaker at longer tasks",
  },
  {
    name: "Qwen 2.5 7B Instruct (Q4_K_M)",
    file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
    url: "https://huggingface.co/bartowski/Qwen2.5-7B-Instruct-GGUF/resolve/main/Qwen2.5-7B-Instruct-Q4_K_M.gguf",
    size: "about 4.7 GB",
    fits: "for 16 GB of memory or more; better answers and tool use",
  },
] as const;

function installCommand(platform: ModelSetupHints["platform"]): string {
  switch (platform) {
    case "windows":
      return "winget install llama.cpp";
    case "macos":
    case "linux":
      return "brew install llama.cpp";
  }
}

function downloadCommand(platform: ModelSetupHints["platform"], file: string, url: string): string {
  return platform === "windows"
    ? `mkdir "%USERPROFILE%\\models" & curl -L -o "%USERPROFILE%\\models\\${file}" ${url}`
    : `mkdir -p ~/models && curl -L -o ~/models/${file} ${url}`;
}

export function describeBytes(bytes: number): string {
  const gib = bytes / 1024 ** 3;
  return gib >= 1 ? `${gib.toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1024 ** 2))} MB`;
}

/** A command to paste into a terminal, with a button that copies it. */
function Command({ text }: { readonly text: string }): React.JSX.Element {
  const [copied, setCopied] = useState(false);
  return (
    <div className="guide-command">
      <code>{text}</code>
      <button
        type="button"
        className="button button--secondary"
        onClick={() => {
          void navigator.clipboard.writeText(text).then(
            () => {
              setCopied(true);
              window.setTimeout(() => {
                setCopied(false);
              }, 1500);
            },
            () => undefined,
          );
        }}
      >
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

function Step({
  number,
  done,
  title,
  children,
}: {
  readonly number: number;
  readonly done: boolean;
  readonly title: string;
  readonly children: React.ReactNode;
}): React.JSX.Element {
  return (
    <li className="guide-step" data-done={done}>
      <span className="guide-number" aria-hidden="true">
        {done ? "✓" : number}
      </span>
      <div>
        <h3>
          {title}
          {done && <span className="sr-only"> (done)</span>}
        </h3>
        {children}
      </div>
    </li>
  );
}

/**
 * How to get a model running on this computer, step by step, checked
 * against what is already here. Tramontana downloads nothing and runs no
 * installer: the guide gives the commands to paste into a terminal, then
 * looks again.
 */
export function ModelGuide({
  hasEndpoint,
}: {
  /** Whether a model is already set up; the guide then starts folded. */
  readonly hasEndpoint: boolean;
}): React.JSX.Element {
  const [hints, setHints] = useState<ModelSetupHints | undefined>();
  const [checking, setChecking] = useState(false);

  const check = useCallback(() => {
    setChecking(true);
    void modelSetupHints()
      .then(setHints, () => undefined)
      .finally(() => {
        setChecking(false);
      });
  }, []);
  // Looked once on opening; "Check again" looks again.
  useEffect(() => {
    let live = true;
    void modelSetupHints().then(
      (found) => {
        if (live) setHints(found);
      },
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, []);

  const platform = hints?.platform ?? "macos";
  const server = hints?.llamaServer ?? undefined;
  const models = hints?.models ?? [];
  const pick = platform === "macos" ? " In the dialog, press ⌘⇧G to type a folder." : "";

  return (
    <details className="settings-card model-guide" open={!hasEndpoint}>
      <summary>
        <Icon name="sparkle" size={16} />
        <span>
          {hasEndpoint ? "How to add another model" : "Get a model running on this computer"}
        </span>
      </summary>
      <p className="settings-hint">
        Tramontana runs models with llama.cpp. It downloads nothing itself: paste these commands
        into a terminal, then check again.
      </p>
      <ol className="guide-steps">
        <Step number={1} done={server !== undefined} title="Install llama.cpp">
          {server === undefined ? (
            <Command text={installCommand(platform)} />
          ) : (
            <p className="guide-found">
              Found <code>{server}</code>
            </p>
          )}
        </Step>
        <Step number={2} done={models.length > 0} title="Download a model">
          {models.length > 0 && (
            <ul className="guide-found-list">
              {models.map((model) => (
                <li key={model.path}>
                  <code>{model.path}</code> <small>{describeBytes(model.bytes)}</small>
                </li>
              ))}
            </ul>
          )}
          {MODELS.map((model) => (
            <div key={model.file} className="guide-model">
              <p>
                <strong>{model.name}</strong>, {model.size}: {model.fits}.
              </p>
              <Command text={downloadCommand(platform, model.file, model.url)} />
            </div>
          ))}
        </Step>
        <Step number={3} done={hasEndpoint} title="Add it to Tramontana">
          <p>
            Below, under <strong>Add an endpoint</strong>, choose{" "}
            <strong>llama-server started by Tramontana</strong>, give it a name, and click{" "}
            <strong>Choose files and add…</strong>. Pick the llama-server program
            {server === undefined ? (
              ""
            ) : (
              <>
                {" "}
                (<code>{server}</code>)
              </>
            )}
            , then the model file.
            {pick}
          </p>
          <p>
            Then tick <strong>Tool calls verified</strong> so Agent tasks can use it, and restart
            Tramontana.
          </p>
        </Step>
      </ol>
      <div className="settings-actions">
        <button
          type="button"
          className="button button--secondary"
          onClick={check}
          disabled={checking}
        >
          {checking ? "Checking…" : "Check again"}
        </button>
        {hints !== undefined && hints.lookedIn.length > 0 && (
          <span className="settings-hint">Looked for models in {hints.lookedIn.join(", ")}.</span>
        )}
      </div>
    </details>
  );
}
