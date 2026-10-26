import { useState } from "react";

import { errorMessage, projectChanges } from "./core.js";
import type { ProjectChanges } from "./core.js";
import { Failure } from "./Failure.js";
import { useLoad } from "./useLoad.js";

/**
 * The project's working tree, as Git sees it (ADR 0032).
 *
 * Read when the panel opens, when the person asks, and again whenever the
 * shell says a task has ended, so the diff a person reviews is the one on disk
 * now rather than one remembered. Every line keeps Git's own marks — `+`,
 * `-`, `@@` — so an added or removed line is told apart by its text, and the
 * colors only repeat it.
 *
 * Both panes are taller than the room they are given and scroll. A scrolling
 * box that cannot be focused can only be read with a pointer, so each is a
 * named region the keyboard can enter: reviewing a change before approving
 * the next one is not something a person should need a mouse for.
 */

type LineKind = "file" | "hunk" | "added" | "removed" | "context";

function classify(line: string): LineKind {
  if (line.startsWith("diff --git") || line.startsWith("+++") || line.startsWith("---")) {
    return "file";
  }
  if (line.startsWith("@@")) return "hunk";
  if (line.startsWith("+")) return "added";
  if (line.startsWith("-")) return "removed";
  return "context";
}

/** What the most recent read settled on, and which read it was. */
interface Settled {
  readonly key: string;
  readonly changes?: ProjectChanges;
  readonly failure?: string;
}

interface ChangesPanelProps {
  readonly projectId: string;
  /** Changed by the shell whenever a task ends, so the panel reads again. */
  readonly revision: number;
}

export function ChangesPanel({ projectId, revision }: ChangesPanelProps): React.JSX.Element {
  const [refreshes, setRefreshes] = useState(0);
  const [settled, setSettled] = useState<Settled | undefined>();

  // Every reason to read — another project, a task that ended, the person
  // asking — makes a new key. The effect only starts the read; a result is
  // kept only if nothing newer has been asked for since.
  const key = `${projectId}\n${revision}\n${refreshes}`;
  useLoad(
    () => projectChanges(projectId),
    [key, projectId],
    (changes) => {
      setSettled({ key, changes });
    },
    (error) => {
      setSettled({ key, failure: errorMessage(error, "Could not read the working tree.") });
    },
  );

  const reading = settled?.key !== key;
  const changes = settled?.changes;

  return (
    <section className="changes" aria-labelledby="changes-heading">
      <div className="section-heading-row">
        <h2 id="changes-heading">Changes</h2>
        <button
          type="button"
          className="button button--secondary"
          disabled={reading}
          onClick={() => {
            setRefreshes((count) => count + 1);
          }}
        >
          {reading ? "Reading…" : "Refresh"}
        </button>
      </div>
      <Failure text={settled?.failure} />
      {changes !== undefined && (
        <>
          <pre className="changes-status" role="region" tabIndex={0} aria-label="Git status">
            {changes.status.trim() === "" ? "Git reported nothing." : changes.status}
          </pre>
          {changes.diff.trim() === "" ? (
            <p className="inline-empty">
              No changes to tracked files. New files, if any, are listed above.
            </p>
          ) : (
            <pre className="changes-diff" role="region" tabIndex={0} aria-label="Git diff">
              {changes.diff.split("\n").map((line, index) => (
                // Diff lines repeat, so their position is their identity.
                <span key={index} className="diff-line" data-line={classify(line)}>
                  {line}
                  {"\n"}
                </span>
              ))}
            </pre>
          )}
          {changes.truncated && (
            <p className="notice" role="note">
              Git&apos;s output was longer than a read may return, so it was cut short.
            </p>
          )}
        </>
      )}
    </section>
  );
}
