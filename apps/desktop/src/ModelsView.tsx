import { useState } from "react";

import { AddEndpointForm } from "./AddEndpointForm.js";
import { ModelGuide, describeBytes } from "./ModelGuide.js";
import {
  errorMessage,
  checkEndpointModel,
  forgetCredential,
  listConfiguredEndpoints,
  listModelEndpoints,
  removeEndpoint,
  restartApplication,
  setEndpointEnabled,
  setEndpointToolCalls,
  storeCredential,
  trustEndpoint,
} from "./core.js";
import type {
  ConfiguredEndpoint,
  CredentialStatus,
  FileStatus,
  ModelEndpointStatus,
  RuntimeFiles,
} from "./core.js";
import { Failure } from "./Failure.js";
import { useLoad, useRestartable } from "./useLoad.js";

/**
 * Models and credentials: what is configured, and whether it is in use.
 *
 * The same settings the CLI's `provider` and `secret` commands change. A
 * change is saved at once but used only once Tramontana restarts, because the
 * running tasks were routed against the models it opened with; the view says
 * so, and offers the restart rather than doing it.
 *
 * A credential is typed into a password field, sent once, and cleared. What
 * comes back is how many characters were stored — never the value — and a
 * store that cannot answer is shown as unable to answer, never as empty.
 */

const KIND_TEXT: Record<ConfiguredEndpoint["kind"], string> = {
  openAiCompatible: "OpenAI-compatible",
  anthropic: "Anthropic",
  llamaServer: "llama-server started by Tramontana",
  mlxServer: "mlx_lm.server started by Tramontana",
};

/** Whether an endpoint is in use now, in words. */
function describeUse(
  endpoint: ConfiguredEndpoint,
  running: ModelEndpointStatus | undefined,
): string {
  if (!endpoint.enabled) return "Turned off";
  if (running === undefined) return "Not in use until Tramontana restarts";
  if (running.available) {
    const where =
      running.accelerators === undefined
        ? ""
        : running.accelerators.length === 0
          ? " · reported no GPU, so it may run on the processor alone"
          : ` · on ${running.accelerators.join(", ")}`;
    return `In use · ${running.models.join(", ")}${where}`;
  }
  return `Unavailable: ${running.unavailableReason ?? "no reason was given"}`;
}

/** A size in words a person reads at a glance. */
/** Where a runtime's executable stands against its record (ADR 0043). */
function describeExecutable(runtime: RuntimeFiles): string {
  switch (runtime.executableStatus) {
    case "confirmed":
      return `${runtime.executable} is as you added or last confirmed it.`;
    case "held":
      return `Held: ${runtime.executable} changed since you added or last confirmed it, and will not be started until you confirm it.`;
    case "unreadable":
      return `Held: ${runtime.executable} could not be read.`;
    case "notRecorded":
      return `${runtime.executable} was added before files were recorded; confirm it to record them.`;
  }
}

const MODEL_CHECK_TEXT: Record<FileStatus, string> = {
  confirmed: "The model is as recorded.",
  held: "The model changed since it was recorded. Confirm the files if you meant to change it.",
  unreadable: "The model could not be read.",
  notRecorded: "The model was not recorded; confirm the files to record it.",
};

function describeCredential(credential: CredentialStatus): string {
  if (credential.configured === undefined) {
    return `could not be checked: ${credential.unavailableReason ?? "the store did not answer"}`;
  }
  return credential.configured ? "stored" : "not stored";
}

interface ModelsViewProps {
  /** Return to the work area. */
  /** Return to the work area; absent when shown inside Settings. */
  readonly onClose?: () => void;
}

export function ModelsView({ onClose }: ModelsViewProps): React.JSX.Element {
  const [configured, setConfigured] = useState<readonly ConfiguredEndpoint[] | undefined>();
  // Asked once: whether "Restart now" would bring the window back, or leave
  // it blank as it does under the development server.
  const restartable = useRestartable();
  const [running, setRunning] = useState<readonly ModelEndpointStatus[]>([]);
  const [failure, setFailure] = useState<string | undefined>();
  const [note, setNote] = useState<string | undefined>();
  const [changed, setChanged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [removing, setRemoving] = useState<string | undefined>();
  const [values, setValues] = useState<Readonly<Record<string, string>>>({});

  // Every change asks for a fresh read by moving this on. The effect only
  // starts the read; what it finds is kept only if nothing newer was asked for.
  const [reads, setReads] = useState(0);
  useLoad(
    () => Promise.all([listConfiguredEndpoints(), listModelEndpoints()]),
    [reads],
    ([endpoints, health]) => {
      setConfigured(endpoints);
      setRunning(health);
    },
    (error) => {
      setFailure(errorMessage(error, "Could not read the model settings."));
    },
  );

  const act = async (
    action: () => Promise<string | undefined>,
    fallback: string,
    changesSettings: boolean,
  ): Promise<void> => {
    setBusy(true);
    setFailure(undefined);
    setNote(undefined);
    try {
      setNote(await action());
      if (changesSettings) setChanged(true);
      setReads((count) => count + 1);
    } catch (error: unknown) {
      setFailure(errorMessage(error, fallback));
    } finally {
      setBusy(false);
    }
  };

  const saveCredential = (key: string): void => {
    const value = values[key] ?? "";
    void act(
      async () => {
        const stored = await storeCredential(key, value);
        // Cleared once sent: the value is not kept in the window.
        setValues((current) => ({ ...current, [key]: "" }));
        return `Stored ${stored} characters for ${key}.`;
      },
      "Could not store the credential.",
      false,
    );
  };

  return (
    <section className="models-view" aria-labelledby="models-heading">
      {onClose === undefined ? (
        <h2 id="models-heading" className="settings-part-heading">
          Models and credentials
        </h2>
      ) : (
        <div className="section-heading-row">
          <h1 id="models-heading">Models and credentials</h1>
          <button type="button" className="button button--secondary" onClick={onClose}>
            Back to work
          </button>
        </div>
      )}

      <Failure text={failure} />
      {note !== undefined && (
        <p className="notice" role="status">
          {note}
        </p>
      )}
      {changed && (
        <div className="notice models-restart" role="note">
          <p>
            Changes are saved, and used once Tramontana restarts. Restarting interrupts running
            tasks; they can be inspected and resumed when it opens again.
          </p>
          {restartable === false ? (
            <p>
              This copy runs under the development server, which stops when Tramontana does, so it
              cannot restart itself: quit it and run the start command again.
            </p>
          ) : (
            <button
              type="button"
              className="button button--primary"
              disabled={restartable === undefined}
              onClick={() => void restartApplication()}
            >
              Restart now
            </button>
          )}
        </div>
      )}

      {configured !== undefined && <ModelGuide hasEndpoint={configured.length > 0} />}

      {configured === undefined ? (
        <p className="inline-empty">Reading the model settings…</p>
      ) : configured.length === 0 ? (
        <p className="inline-empty">
          No model endpoints are configured, so tasks run against a labelled stand-in model.
        </p>
      ) : (
        <ul className="endpoint-list" aria-label="Configured endpoints">
          {configured.map((endpoint) => {
            const health = running.find((status) => status.endpointId === endpoint.endpointId);
            const credential = endpoint.credential;
            return (
              <li key={endpoint.endpointId} className="endpoint">
                <h2>{endpoint.displayName}</h2>
                <p>
                  {KIND_TEXT[endpoint.kind]} · <code>{endpoint.modelId}</code> ·{" "}
                  <span data-leaves={endpoint.leavesTheDevice}>
                    {endpoint.leavesTheDevice ? "Leaves this device" : "Runs on this device"}
                  </span>
                </p>
                <p className="endpoint-use">{describeUse(endpoint, health)}</p>
                {endpoint.runtime !== undefined && (
                  <div className="endpoint-credential">
                    <p>{describeExecutable(endpoint.runtime)}</p>
                    {endpoint.runtime.modelBytes !== undefined && (
                      <p className="card-meta">
                        Model {describeBytes(endpoint.runtime.modelBytes)}, so it needs at least
                        that much memory. Recorded {endpoint.runtime.recordedAt}.
                      </p>
                    )}
                    <div className="endpoint-actions">
                      <button
                        type="button"
                        className="button button--secondary"
                        disabled={busy}
                        onClick={() =>
                          void act(
                            async () =>
                              MODEL_CHECK_TEXT[await checkEndpointModel(endpoint.endpointId)],
                            "Could not check the model.",
                            false,
                          )
                        }
                      >
                        Check the model
                      </button>
                      <button
                        type="button"
                        className={
                          endpoint.runtime.executableStatus === "confirmed"
                            ? "button button--secondary"
                            : "button button--primary"
                        }
                        disabled={busy}
                        onClick={() =>
                          void act(
                            async () => {
                              await trustEndpoint(endpoint.endpointId);
                              return `Confirmed the files of ${endpoint.displayName} as they are now.`;
                            },
                            "Could not confirm the files.",
                            true,
                          )
                        }
                      >
                        Confirm files
                      </button>
                    </div>
                  </div>
                )}

                <div className="endpoint-switches">
                  <label>
                    <input
                      type="checkbox"
                      checked={endpoint.enabled}
                      disabled={busy}
                      onChange={() =>
                        void act(
                          async () => {
                            await setEndpointEnabled(endpoint.endpointId, !endpoint.enabled);
                            return undefined;
                          },
                          "Could not change the endpoint.",
                          true,
                        )
                      }
                    />
                    Enabled
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={endpoint.supportsToolCalls}
                      disabled={busy}
                      onChange={() =>
                        void act(
                          async () => {
                            await setEndpointToolCalls(
                              endpoint.endpointId,
                              !endpoint.supportsToolCalls,
                            );
                            return undefined;
                          },
                          "Could not change the endpoint.",
                          true,
                        )
                      }
                    />
                    Tool calls verified
                  </label>
                </div>

                {credential !== undefined && (
                  <div className="endpoint-credential">
                    <p>
                      Credential <code>{credential.key}</code>: {describeCredential(credential)}
                    </p>
                    <label>
                      New value for {credential.key}
                      <input
                        type="password"
                        autoComplete="off"
                        spellCheck={false}
                        value={values[credential.key] ?? ""}
                        onChange={(changed) => {
                          const typed = changed.target.value;
                          setValues((current) => ({ ...current, [credential.key]: typed }));
                        }}
                      />
                    </label>
                    <div className="endpoint-actions">
                      <button
                        type="button"
                        className="button button--primary"
                        disabled={busy || (values[credential.key] ?? "") === ""}
                        onClick={() => {
                          saveCredential(credential.key);
                        }}
                      >
                        Save credential
                      </button>
                      <button
                        type="button"
                        className="button button--secondary"
                        disabled={busy}
                        onClick={() =>
                          void act(
                            async () =>
                              (await forgetCredential(credential.key))
                                ? `Removed ${credential.key} from the store.`
                                : `Nothing was stored for ${credential.key}.`,
                            "Could not remove the credential.",
                            false,
                          )
                        }
                      >
                        Forget credential
                      </button>
                    </div>
                  </div>
                )}

                <div className="endpoint-actions">
                  {removing === endpoint.endpointId ? (
                    <>
                      <span>Remove {endpoint.displayName}? Its credential stays in the store.</span>
                      <button
                        type="button"
                        className="button button--secondary"
                        disabled={busy}
                        onClick={() =>
                          void act(
                            async () => {
                              await removeEndpoint(endpoint.endpointId);
                              setRemoving(undefined);
                              return `Removed ${endpoint.displayName}.`;
                            },
                            "Could not remove the endpoint.",
                            true,
                          )
                        }
                      >
                        Remove
                      </button>
                      <button
                        type="button"
                        className="button button--secondary"
                        onClick={() => {
                          setRemoving(undefined);
                        }}
                      >
                        Keep
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      className="button button--secondary"
                      disabled={busy}
                      onClick={() => {
                        setRemoving(endpoint.endpointId);
                      }}
                    >
                      Remove…
                    </button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}

      <AddEndpointForm
        onAdded={(message) => {
          setFailure(undefined);
          setNote(message);
          setChanged(true);
          setReads((count) => count + 1);
        }}
      />
    </section>
  );
}
