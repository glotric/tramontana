import { useState } from "react";

import { errorMessage, eraseAllData, restartApplication } from "./core.js";
import { Failure } from "./Failure.js";
import { useRestartable } from "./useLoad.js";

/** What the person types to confirm. */
const CONFIRMATION = "delete";

/**
 * Settings' "Delete all my data" (ADR 0068): everything Tramontana keeps on
 * this computer, gone at once, after the person says exactly what they mean.
 * The application restarts afterwards, as new.
 */
export function EraseAllData(): React.JSX.Element {
  const [confirming, setConfirming] = useState(false);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | undefined>();
  const [erased, setErased] = useState(false);
  // Asked once: whether a restart would bring the window back, or leave it
  // blank as it does under the development server.
  const restartable = useRestartable();

  const cancel = (): void => {
    setConfirming(false);
    setTyped("");
    setFailure(undefined);
  };

  const erase = (): void => {
    setBusy(true);
    setFailure(undefined);
    void eraseAllData().then(
      () => {
        setErased(true);
        if (restartable === true) void restartApplication();
      },
      (error: unknown) => {
        setBusy(false);
        setFailure(errorMessage(error, "Could not delete your data."));
      },
    );
  };

  return (
    <section className="settings-section" aria-labelledby="erase-heading">
      <h2 id="erase-heading">Delete all my data</h2>
      <p className="settings-hint">
        Removes everything Tramontana keeps on this computer, and starts it again as new. Your
        project folders and the files in them are not touched.
      </p>

      {erased ? (
        <div className="notice" role="status">
          {restartable === true ? (
            <p>Everything was deleted. Tramontana is restarting.</p>
          ) : (
            <p>
              Everything was deleted. Quit Tramontana and start it again: this copy runs under the
              development server, which cannot restart itself.
            </p>
          )}
        </div>
      ) : confirming ? (
        <div className="erase-confirm" role="alertdialog" aria-labelledby="erase-question">
          <p id="erase-question">
            <strong>Delete everything? This can&apos;t be undone.</strong>
          </p>
          <ul>
            <li>Your projects list, conversations, tasks and their records</li>
            <li>Schedules, personal details and notes</li>
            <li>Model endpoints, Apps and dictation settings</li>
            <li>API keys and sign-ins those settings use, from the system keychain</li>
            <li>Copies of files kept so deletes could be undone, and the browser&apos;s data</li>
          </ul>
          <p className="settings-hint">
            A key whose endpoint you removed earlier is not known any more and stays in the
            keychain, under Tramontana. Running tasks must be stopped first.
          </p>
          <label htmlFor="erase-confirmation">
            Type <strong>{CONFIRMATION}</strong> to confirm
          </label>
          <input
            id="erase-confirmation"
            value={typed}
            autoComplete="off"
            onChange={(changed) => {
              setTyped(changed.target.value);
            }}
          />
          <Failure text={failure} />
          <div className="menu-actions">
            <button
              type="button"
              className="button button--secondary"
              disabled={busy}
              onClick={cancel}
            >
              Cancel
            </button>
            <button
              type="button"
              className="button button--danger"
              disabled={busy || typed.trim().toLowerCase() !== CONFIRMATION}
              onClick={erase}
            >
              {busy ? "Deleting…" : "Delete everything"}
            </button>
          </div>
        </div>
      ) : (
        <div className="menu-actions erase-start">
          <button
            type="button"
            className="button button--danger"
            onClick={() => {
              setConfirming(true);
            }}
          >
            Delete all my data…
          </button>
        </div>
      )}
    </section>
  );
}
