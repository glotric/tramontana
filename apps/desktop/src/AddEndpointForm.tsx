import { useState } from "react";

import {
  errorMessage,
  addAnthropicEndpoint,
  addLlamaEndpoint,
  addMlxEndpoint,
  addOpenAiEndpoint,
} from "./core.js";
import { Failure } from "./Failure.js";

/**
 * Adding a model endpoint, as `tramontana provider add-…` does.
 *
 * A llama-server comes first because Tramontana is local by default; its
 * executable and model file are chosen in the native picker, never typed or
 * searched for. A credential is named here by its key and entered afterwards,
 * on the endpoint, so it never travels with the rest of the form.
 *
 * The shell refuses what it must — plain http to anything off this device, a
 * name already in use — and the form shows its reason. What comes back is
 * whether work sent to the new endpoint leaves the device, and the form says
 * so, because that is what decides where a person's data goes.
 */

type Kind = "llamaServer" | "mlxServer" | "openAiCompatible" | "anthropic";

/** The kinds Tramontana starts itself, from files the person picks. */
const STARTED: ReadonlySet<Kind> = new Set<Kind>(["llamaServer", "mlxServer"]);

const KINDS: readonly (readonly [Kind, string])[] = [
  ["llamaServer", "llama-server started by Tramontana"],
  ["mlxServer", "mlx_lm.server started by Tramontana (Apple Silicon)"],
  ["openAiCompatible", "OpenAI-compatible server"],
  ["anthropic", "Anthropic"],
];

const ANTHROPIC_ADDRESS = "https://api.anthropic.com";

interface AddEndpointFormProps {
  /** Called with a sentence saying what was added and where it runs. */
  readonly onAdded: (message: string) => void;
}

export function AddEndpointForm({ onAdded }: AddEndpointFormProps): React.JSX.Element {
  const [kind, setKind] = useState<Kind>("llamaServer");
  const [endpointId, setEndpointId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [modelId, setModelId] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [credentialKey, setCredentialKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | undefined>();
  const [note, setNote] = useState<string | undefined>();

  const address = kind === "anthropic" && baseUrl === "" ? ANTHROPIC_ADDRESS : baseUrl;
  const ready =
    endpointId.trim() !== "" &&
    modelId.trim() !== "" &&
    (STARTED.has(kind) || address.trim() !== "") &&
    (kind !== "anthropic" || credentialKey.trim() !== "");

  const submit = async (): Promise<void> => {
    setBusy(true);
    setFailure(undefined);
    setNote(undefined);
    const common = {
      endpointId: endpointId.trim(),
      modelId: modelId.trim(),
      ...(displayName.trim() === "" ? {} : { displayName: displayName.trim() }),
    };
    try {
      let leaves: boolean | undefined;
      if (kind === "llamaServer") {
        leaves = await addLlamaEndpoint(common);
      } else if (kind === "mlxServer") {
        leaves = await addMlxEndpoint(common);
      } else if (kind === "anthropic") {
        leaves = await addAnthropicEndpoint({
          ...common,
          baseUrl: address.trim(),
          credentialKey: credentialKey.trim(),
        });
      } else {
        leaves = await addOpenAiEndpoint({
          ...common,
          baseUrl: address.trim(),
          ...(credentialKey.trim() === "" ? {} : { credentialKey: credentialKey.trim() }),
        });
      }
      if (leaves === undefined) {
        setNote("Nothing was added: nothing was chosen.");
        return;
      }
      onAdded(
        `Added ${common.endpointId}. Work sent to it ${
          leaves ? "leaves this device" : "runs on this device"
        }.`,
      );
      setEndpointId("");
      setDisplayName("");
      setModelId("");
      setBaseUrl("");
      setCredentialKey("");
    } catch (error: unknown) {
      setFailure(errorMessage(error, "Could not add the endpoint."));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className="add-endpoint"
      aria-labelledby="add-endpoint-heading"
      onSubmit={(submitted) => {
        submitted.preventDefault();
        void submit();
      }}
    >
      <h2 id="add-endpoint-heading">Add an endpoint</h2>

      <fieldset className="add-endpoint-kinds">
        <legend>Kind</legend>
        {KINDS.map(([value, label]) => (
          <label key={value}>
            <input
              type="radio"
              name="endpoint-kind"
              value={value}
              checked={kind === value}
              onChange={() => {
                setKind(value);
              }}
            />
            {label}
          </label>
        ))}
      </fieldset>

      <label>
        Name
        <input
          value={endpointId}
          onChange={(changed) => {
            setEndpointId(changed.target.value);
          }}
          placeholder="local-qwen"
          spellCheck={false}
        />
      </label>
      <label>
        Display name (optional)
        <input
          value={displayName}
          onChange={(changed) => {
            setDisplayName(changed.target.value);
          }}
        />
      </label>
      <label>
        Model
        <input
          value={modelId}
          onChange={(changed) => {
            setModelId(changed.target.value);
          }}
          spellCheck={false}
        />
      </label>

      {kind === "llamaServer" ? (
        <p className="add-endpoint-hint">
          The llama-server executable and the GGUF model file are chosen in the next two dialogs.
          Nothing is downloaded, and your PATH is not searched.
        </p>
      ) : kind === "mlxServer" ? (
        <p className="add-endpoint-hint">
          The mlx_lm.server executable, usually in a virtual environment where you installed mlx-lm,
          and the MLX model folder are chosen in the next two dialogs. It runs only on a Mac with
          Apple Silicon. Nothing is downloaded, and your PATH is not searched.
        </p>
      ) : (
        <>
          <label>
            Address
            <input
              value={address}
              onChange={(changed) => {
                setBaseUrl(changed.target.value);
              }}
              placeholder="http://127.0.0.1:8080"
              spellCheck={false}
            />
          </label>
          <label>
            {kind === "anthropic" ? "Credential key" : "Credential key (optional)"}
            <input
              value={credentialKey}
              onChange={(changed) => {
                setCredentialKey(changed.target.value);
              }}
              placeholder={kind === "anthropic" ? "anthropic.apiKey" : "openai.apiKey"}
              spellCheck={false}
            />
          </label>
          <p className="add-endpoint-hint">
            The key is only a name. Enter the credential itself on the endpoint once it is added.
          </p>
        </>
      )}

      <Failure text={failure} />
      {note !== undefined && (
        <p className="notice" role="note">
          {note}
        </p>
      )}

      <button type="submit" className="button button--primary" disabled={busy || !ready}>
        {STARTED.has(kind) ? "Choose files and add…" : "Add endpoint"}
      </button>
    </form>
  );
}
