import { AppsView } from "./AppsView.js";
import { SkillsSettings } from "./SkillsSettings.js";

/**
 * Settings, Customize: what Tramontana is extended with. Skills are
 * instructions a task follows when chosen with `/` (ADR 0070). Apps are the
 * connectors (MCP servers) its tasks may use, each only when a task is
 * allowed to and with every tool call put to the policy.
 */
export function CustomizeSettings(): React.JSX.Element {
  return (
    <div className="settings-parts">
      <SkillsSettings />
      <div className="settings-card">
        <AppsView />
      </div>
    </div>
  );
}
