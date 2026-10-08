import { AppsView } from "./AppsView.js";

/**
 * Settings, Customize: what Tramontana is extended with. Apps are the
 * connectors (MCP servers) its tasks may use, each only when a task is
 * allowed to and with every tool call put to the policy.
 */
export function CustomizeSettings({
  skills,
}: {
  /** The skills part, when there is one to show. */
  readonly skills?: React.ReactNode;
}): React.JSX.Element {
  return (
    <div className="settings-parts">
      {skills}
      <div className="settings-card">
        <AppsView />
      </div>
    </div>
  );
}
