import { useEffect, useRef } from "react";

import { Icon } from "./icons.js";
import type { CommandRun } from "./terminal.js";

const STATUS_TEXT: Record<CommandRun["status"], string> = {
  waiting: "waiting for your approval",
  refused: "not run",
  running: "running…",
  done: "done",
  failed: "failed",
};

function statusLine(run: CommandRun): string {
  if (run.status === "done" || run.status === "failed") {
    const code = typeof run.exitCode === "number" ? ` · exit ${run.exitCode}` : "";
    return `${STATUS_TEXT[run.status]}${code}`;
  }
  return STATUS_TEXT[run.status];
}

/**
 * The commands the task on screen ran, as a terminal shows them. It reads
 * the task's record and runs nothing itself: every command here was asked
 * about and approved (or refused) like any other, and its output is what
 * the record kept.
 */
export function CommandTerminal({
  runs,
  onClose,
}: {
  readonly runs: readonly CommandRun[];
  readonly onClose: () => void;
}): React.JSX.Element {
  const body = useRef<HTMLDivElement>(null);

  // Follow the newest output, as a terminal does.
  useEffect(() => {
    const element = body.current;
    if (element !== null) element.scrollTop = element.scrollHeight;
  }, [runs]);

  return (
    <section className="terminal" aria-label="Terminal">
      <header className="terminal-bar">
        <Icon name="terminal" size={15} />
        <strong>Terminal</strong>
        <span>{runs.length === 1 ? "1 command" : `${runs.length} commands`} in this task</span>
        <button
          type="button"
          className="terminal-close"
          aria-label="Close terminal"
          onClick={onClose}
        >
          <Icon name="close" size={13} strokeWidth={2.2} />
        </button>
      </header>
      <div ref={body} className="terminal-body" role="log" aria-live="polite">
        {runs.map((run) => (
          <div key={run.invocationId} className="terminal-run" data-status={run.status}>
            <div className="terminal-command">
              <span aria-hidden="true">$</span> {run.command}
            </div>
            {run.stdout !== undefined && <pre className="terminal-out">{run.stdout}</pre>}
            {run.stderr !== undefined && (
              <pre className="terminal-out terminal-err">{run.stderr}</pre>
            )}
            <div className="terminal-status">
              {statusLine(run)}
              {run.truncated === true && " · output shortened"}
              {run.status !== "done" && run.note !== undefined && ` · ${run.note}`}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
