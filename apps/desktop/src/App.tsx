import { useEffect, useState } from "react";
import { isSchemaCompatible, SCHEMA_VERSION } from "@tramontana/shared";

import { fetchCoreInfo } from "./core.js";

/**
 * Result of the startup handshake with the Rust core.
 *
 * `unavailable` is a normal development state, not an error: `pnpm dev:ui`
 * serves this interface in an ordinary browser, where no Tauri IPC exists.
 */
type CoreStatus =
  | { readonly kind: "connecting" }
  | { readonly kind: "ready"; readonly appVersion: string }
  | { readonly kind: "incompatible"; readonly reported: number }
  | { readonly kind: "unavailable" };

function describeCoreStatus(status: CoreStatus): string {
  switch (status.kind) {
    case "connecting":
      return "Connecting to core…";
    case "ready":
      return `Core ${status.appVersion} · contracts v${SCHEMA_VERSION}`;
    case "incompatible":
      return `Core speaks contracts v${status.reported}; this build speaks v${SCHEMA_VERSION}`;
    case "unavailable":
      return "Core unavailable — interface running outside the desktop shell";
  }
}

/**
 * The M0 static shell.
 *
 * Its only job is to prove the interface, the Tauri adapter, and
 * `tramontana-core` are one running process. Projects, tasks, activity, and
 * the rest of the work surface arrive in M1 and M8
 * (`docs/BUILD_ORDER.md` §8, §18).
 */
export function App(): React.JSX.Element {
  const [status, setStatus] = useState<CoreStatus>({ kind: "connecting" });

  useEffect(() => {
    let cancelled = false;

    void fetchCoreInfo().then(
      (info) => {
        if (cancelled) return;
        setStatus(
          isSchemaCompatible(info.schemaVersion)
            ? { kind: "ready", appVersion: info.appVersion }
            : { kind: "incompatible", reported: info.schemaVersion },
        );
      },
      () => {
        if (!cancelled) setStatus({ kind: "unavailable" });
      },
    );

    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <main className="shell">
      <h1 className="shell__brand">Tramontana</h1>
      <p className="shell__tagline">Portable, policy-governed operating layer for AI work.</p>
      <p className="shell__note">
        Milestone M0 — workspace bootstrap. This shell verifies the core handshake and nothing else.
        See <code>docs/BUILD_ORDER.md</code> for what arrives next.
      </p>
      <p className="shell__status" data-state={status.kind} role="status">
        {describeCoreStatus(status)}
      </p>
    </main>
  );
}
