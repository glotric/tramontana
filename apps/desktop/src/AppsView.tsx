import { useEffect, useState } from "react";

import {
  appPermissionHistory,
  asErrorEnvelope,
  listApps,
  restartApplication,
  restartSupported,
  setAppEnabled,
} from "./core.js";
import type { AppHistory, AppStatus, AppView } from "./core.js";

/**
 * Apps: the MCP servers this profile starts, what each offers, how it is
 * doing, and what was decided about its tools (ADR 0040).
 *
 * The same settings the CLI's `app` commands change, less adding and
 * removing, which stay in the CLI. Turning an App on or off is saved at once
 * and takes effect when Tramontana restarts, because its tools were
 * registered with the ones the running tasks may use.
 *
 * What an App says about itself is shown as its own claim, as the approval
 * question shows it. Nothing it is given is shown by value, and its program
 * only by file name.
 */

function describeStatus(status: AppStatus): string {
  switch (status.kind) {
    case "running":
      return "Running";
    case "stopped":
      return `Stopped: ${status.reason}`;
    case "didNotStart":
      return `Did not start: ${status.reason}`;
    case "off":
      return "Turned off";
    case "startsOnRestart":
      return "Turned on; starts when Tramontana restarts";
    case "stopsOnRestart":
      return "Turned off; still running until Tramontana restarts";
  }
}

const GRANT_TEXT: Record<NonNullable<AppHistory["decisions"][number]["grantKind"]>, string> = {
  implicitSafeDefault: "allowed without asking",
  once: "allowed once",
  project: "allowed in this project",
  site: "allowed on this site for the task",
};

function History({ appId }: { readonly appId: string }): React.JSX.Element {
  const [history, setHistory] = useState<AppHistory | undefined>();
  const [failure, setFailure] = useState<string | undefined>();
  useEffect(() => {
    let live = true;
    appPermissionHistory(appId).then(
      (found) => {
        if (live) setHistory(found);
      },
      (error: unknown) => {
        if (live) setFailure(asErrorEnvelope(error)?.message ?? "Could not read the history.");
      },
    );
    return () => {
      live = false;
    };
  }, [appId]);

  if (failure !== undefined) {
    return (
      <p className="notice notice--error" role="alert">
        {failure}
      </p>
    );
  }
  if (history === undefined) return <p className="inline-empty">Reading the record…</p>;
  if (history.decisions.length === 0) {
    return <p className="inline-empty">No task has asked to use this App&apos;s tools.</p>;
  }
  return (
    <>
      <ol className="app-history" aria-label={`Decisions about ${appId}`}>
        {history.decisions.map((decision) => (
          <li key={`${decision.taskId}:${decision.decidedAt}:${decision.toolId}`}>
            <code>{decision.toolId}</code>{" "}
            <span>
              {decision.allowed
                ? decision.grantKind === undefined
                  ? "allowed"
                  : GRANT_TEXT[decision.grantKind]
                : `refused (${decision.reasonCode})`}
            </span>
            {decision.outcome !== undefined && <span> · {decision.outcome}</span>}
            <p className="card-meta">
              {decision.goal} · {decision.decidedAt}
            </p>
          </li>
        ))}
      </ol>
      {history.partial && (
        <p className="notice" role="note">
          Only the most recent decisions are shown; older tasks were not searched.
        </p>
      )}
    </>
  );
}

interface AppsViewProps {
  /** Return to the work area. */
  /** Return to the work area; absent when shown inside Settings. */
  readonly onClose?: () => void;
}

export function AppsView({ onClose }: AppsViewProps): React.JSX.Element {
  const [apps, setApps] = useState<readonly AppView[] | undefined>();
  const [failure, setFailure] = useState<string | undefined>();
  const [changed, setChanged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [showing, setShowing] = useState<string | undefined>();
  const [restartable, setRestartable] = useState<boolean | undefined>();
  const [reads, setReads] = useState(0);

  useEffect(() => {
    let live = true;
    restartSupported().then(
      (supported) => {
        if (live) setRestartable(supported);
      },
      () => {
        if (live) setRestartable(false);
      },
    );
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    let current = true;
    listApps().then(
      (found) => {
        if (current) setApps(found);
      },
      (error: unknown) => {
        if (current) setFailure(asErrorEnvelope(error)?.message ?? "Could not read the Apps.");
      },
    );
    return () => {
      current = false;
    };
  }, [reads]);

  const toggle = (app: AppView): void => {
    setBusy(true);
    setFailure(undefined);
    setAppEnabled(app.appId, !app.enabled).then(
      () => {
        setChanged(true);
        setBusy(false);
        setReads((count) => count + 1);
      },
      (error: unknown) => {
        setFailure(asErrorEnvelope(error)?.message ?? "Could not change the App.");
        setBusy(false);
      },
    );
  };

  return (
    <section className="models-view" aria-labelledby="apps-heading">
      {onClose === undefined ? (
        <h2 id="apps-heading" className="settings-part-heading">
          Apps
        </h2>
      ) : (
        <div className="section-heading-row">
          <h1 id="apps-heading">Apps</h1>
          <button type="button" className="button button--secondary" onClick={onClose}>
            Back to work
          </button>
        </div>
      )}
      <p className="inline-empty">
        MCP servers Tramontana starts. A task offers their tools to a model only when you allow Apps
        for it, and every call is still put to you. Add or remove an App with{" "}
        <code>tramontana app</code>.
      </p>

      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}
      {changed && (
        <div className="notice models-restart" role="note">
          <p>Changes are saved, and take effect once Tramontana restarts.</p>
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

      {apps === undefined ? (
        <p className="inline-empty">Reading the Apps…</p>
      ) : apps.length === 0 ? (
        <p className="inline-empty">
          No Apps are configured. <code>tramontana app add</code> adds one.
        </p>
      ) : (
        <ul className="endpoint-list" aria-label="Configured Apps">
          {apps.map((app) => (
            <li key={app.appId} className="endpoint">
              <h2>{app.displayName}</h2>
              <p>
                <code>{app.appId}</code> ·{" "}
                {app.address === undefined ? (
                  <>
                    runs <code>{app.program}</code>
                  </>
                ) : (
                  <>
                    reached at <code>{app.address}</code>
                  </>
                )}
                {app.protocolVersion !== undefined && <> · MCP {app.protocolVersion}</>}
              </p>
              <p className="endpoint-use">{describeStatus(app.status)}</p>
              <div className="endpoint-switches">
                <label>
                  <input
                    type="checkbox"
                    checked={app.enabled}
                    disabled={busy}
                    onChange={() => {
                      toggle(app);
                    }}
                  />
                  Enabled
                </label>
              </div>
              {(app.variables.length > 0 || app.secrets.length > 0) && (
                <p className="card-meta">
                  Given{" "}
                  {[...app.variables, ...app.secrets.map((name) => `${name} (secret)`)].join(", ")};
                  values are not shown.
                </p>
              )}

              {app.tools.length > 0 && (
                <>
                  <h3>Tools</h3>
                  <ul aria-label={`Tools of ${app.displayName}`}>
                    {app.tools.map((tool) => (
                      <li key={tool.toolId}>
                        <code>{tool.toolId}</code> — {tool.description}{" "}
                        <span className="card-meta">
                          {tool.offered
                            ? "The App says it only reads, so a task that allows Apps may offer it. Not verified."
                            : "The App does not say it only reads, so it is never offered to a model."}
                        </span>
                      </li>
                    ))}
                  </ul>
                </>
              )}
              {app.refused.length > 0 && (
                <>
                  <h3>Refused</h3>
                  <ul aria-label={`Tools refused from ${app.displayName}`}>
                    {app.refused.map((refusal) => (
                      <li key={refusal.name}>
                        <code>{refusal.name}</code>: {refusal.reason}
                      </li>
                    ))}
                  </ul>
                </>
              )}
              {app.diagnostics !== "" && (
                <>
                  <h3>Diagnostics</h3>
                  <pre
                    className="changes-status"
                    role="region"
                    tabIndex={0}
                    aria-label={`What ${app.displayName} last wrote to its error stream`}
                  >
                    {app.diagnostics}
                  </pre>
                </>
              )}

              <div className="endpoint-actions">
                <button
                  type="button"
                  className="button button--secondary"
                  aria-expanded={showing === app.appId}
                  onClick={() => {
                    setShowing((current) => (current === app.appId ? undefined : app.appId));
                  }}
                >
                  {showing === app.appId ? "Hide decisions" : "Show decisions"}
                </button>
              </div>
              {showing === app.appId && <History appId={app.appId} />}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
