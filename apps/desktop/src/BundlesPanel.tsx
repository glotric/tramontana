import { useState } from "react";
import type { ExportManifest, OmissionCategory } from "@tramontana/shared";

import { asErrorEnvelope, exportBundle, importStagedBundle, inspectBundle } from "./core.js";
import type { BundleCounts, ExportReport, ImportReport, StagedBundle } from "./core.js";

/**
 * Export and import, as the CLI's `bundle` commands do them.
 *
 * Export is a release of data, so it withholds absolute paths and raw tool
 * output unless the person asks, and afterwards says what it left out. Import
 * treats a bundle as untrusted: the person sees what it holds before anything
 * is written, confirms, and is told that nothing in it was run. Every folder
 * comes from the native picker; this panel never names one.
 */

const OMISSION_TEXT: Record<OmissionCategory, string> = {
  secret: "secret(s)",
  absolutePath: "absolute path(s)",
  rawToolOutput: "raw tool output(s)",
  artifact: "artifact(s)",
  internalEvent: "internal event(s)",
  workerPrivateState: "piece(s) of worker state",
};

function leftOut(manifest: ExportManifest): string {
  const parts = manifest.omissions
    .filter((omission) => omission.count > 0)
    .map((omission) => `${omission.count} ${OMISSION_TEXT[omission.category]}`);
  return parts.length === 0 ? "Nothing was left out." : `Left out: ${parts.join(", ")}.`;
}

/**
 * An import that met a record this profile already holds. The core names the
 * record by its identifier; what a person needs is what that means.
 */
const ALREADY_HERE =
  "This profile already holds this bundle's records, so the import stopped. A bundle can be " +
  "imported into a profile once; to look at it separately, start Tramontana on another profile.";

function describeCounts(counts: BundleCounts): string {
  return (
    `${counts.tasks} task(s), ${counts.events} event(s), ` +
    `${counts.conversations} conversation(s), ${counts.projects} project(s)`
  );
}

interface BundlesPanelProps {
  /**
   * The open project, which can be exported. Absent in a profile with no
   * project yet, where importing one is the only thing to do.
   */
  readonly projectId: string | undefined;
  /** The task on screen, which can be exported on its own. */
  readonly taskId: string | undefined;
  /** Called after an import wrote records, so the shell can reload its lists. */
  readonly onImported: () => void;
}

export function BundlesPanel({
  projectId,
  taskId,
  onImported,
}: BundlesPanelProps): React.JSX.Element {
  const [includeAbsolutePaths, setIncludeAbsolutePaths] = useState(false);
  const [includeRawToolOutput, setIncludeRawToolOutput] = useState(false);
  const [exported, setExported] = useState<ExportReport | undefined>();
  const [staged, setStaged] = useState<StagedBundle | undefined>();
  const [imported, setImported] = useState<ImportReport | undefined>();
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | undefined>();

  const run = async (
    action: () => Promise<void>,
    fallback: string,
    conflict?: string,
  ): Promise<void> => {
    setBusy(true);
    setFailure(undefined);
    try {
      await action();
    } catch (error: unknown) {
      const envelope = asErrorEnvelope(error);
      setFailure(
        envelope?.code === "conflict" && conflict !== undefined
          ? conflict
          : (envelope?.message ?? fallback),
      );
    } finally {
      setBusy(false);
    }
  };

  const onExport = (scope: { readonly taskId: string } | { readonly projectId: string }): void => {
    void run(async () => {
      setImported(undefined);
      const report = await exportBundle(scope, { includeAbsolutePaths, includeRawToolOutput });
      if (report !== undefined) setExported(report);
    }, "Could not write the bundle.");
  };

  const onInspect = (): void => {
    void run(async () => {
      setExported(undefined);
      setImported(undefined);
      const inspected = await inspectBundle();
      if (inspected !== undefined) setStaged(inspected);
    }, "Could not read the bundle.");
  };

  const onImport = (): void => {
    if (staged === undefined) return;
    void run(
      async () => {
        const report = await importStagedBundle(staged.stagingId);
        setStaged(undefined);
        setImported(report);
        onImported();
      },
      "Could not import the bundle.",
      ALREADY_HERE,
    );
  };

  return (
    <section className="bundles" aria-labelledby="bundles-heading">
      <h2 id="bundles-heading">Bundles</h2>

      {projectId !== undefined && (
        <div className="bundle-options">
          <label>
            <input
              type="checkbox"
              checked={includeAbsolutePaths}
              onChange={(changed) => {
                setIncludeAbsolutePaths(changed.target.checked);
              }}
            />
            Keep absolute paths (they name this machine)
          </label>
          <label>
            <input
              type="checkbox"
              checked={includeRawToolOutput}
              onChange={(changed) => {
                setIncludeRawToolOutput(changed.target.checked);
              }}
            />
            Keep raw tool output (it can hold file contents)
          </label>
        </div>
      )}

      <div className="bundle-actions">
        {projectId !== undefined && (
          <>
            <button
              type="button"
              className="button button--secondary"
              disabled={busy}
              onClick={() => {
                onExport({ projectId });
              }}
            >
              Export project…
            </button>
            <button
              type="button"
              className="button button--secondary"
              disabled={busy || taskId === undefined}
              onClick={() => {
                if (taskId !== undefined) onExport({ taskId });
              }}
            >
              Export this task…
            </button>
          </>
        )}
        <button
          type="button"
          className="button button--secondary"
          disabled={busy}
          onClick={onInspect}
        >
          Import a bundle…
        </button>
      </div>

      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}

      {exported !== undefined && (
        <p className="bundle-result" role="status">
          Written to <code>{exported.directory}</code>. {leftOut(exported.manifest)}
        </p>
      )}

      {staged !== undefined && (
        <div className="bundle-staged" role="group" aria-label="Bundle to import">
          <p>
            This bundle holds {describeCounts(staged.inspection.contains)}. Nothing has been written
            yet. {leftOut(staged.inspection.manifest)}
          </p>
          {staged.inspection.contains.quarantined > 0 && (
            <p className="notice" role="note">
              {staged.inspection.contains.quarantined} piece(s) of worker state will be refused
              rather than restored.
            </p>
          )}
          <div className="bundle-actions">
            <button
              type="button"
              className="button button--primary"
              disabled={busy}
              onClick={onImport}
            >
              Import into this profile
            </button>
            <button
              type="button"
              className="button button--secondary"
              disabled={busy}
              onClick={() => {
                setStaged(undefined);
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {imported !== undefined && (
        <p className="bundle-result" role="status">
          Imported {describeCounts(imported.imported)}.{" "}
          {imported.executedAnything ? "" : "Nothing in the bundle was run."}
        </p>
      )}
    </section>
  );
}
