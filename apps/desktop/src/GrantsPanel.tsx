import { useEffect, useState } from "react";
import type { ProjectGrant, RiskClass } from "@tramontana/shared";

import { asErrorEnvelope, listProjectGrants, revokeProjectGrant } from "./core.js";

/**
 * Standing permissions: what a person allowed "in this project", and a way to
 * take it back.
 *
 * A grant is made by answering an approval with "Allow in this project", and
 * covers later requests for the same tool and capability, up to the risk it
 * was granted at and never beyond (`SECURITY_AND_PERMISSIONS.md` §4). Showing
 * them is what makes "revocable" true in practice: a permission nobody can
 * find is one nobody will withdraw.
 */

/** The highest risk a grant covers, as the most it lets a tool do. */
const CEILING_TEXT: Record<RiskClass, string> = {
  read: "reading",
  modify: "changing files",
  dangerous: "running commands",
};

interface Settled {
  readonly key: string;
  readonly grants?: readonly ProjectGrant[];
  readonly failure?: string;
}

interface GrantsPanelProps {
  readonly projectId: string;
  /** Changed by the shell when an approval is answered, so the panel reads again. */
  readonly revision: number;
}

export function GrantsPanel({ projectId, revision }: GrantsPanelProps): React.JSX.Element {
  const [reads, setReads] = useState(0);
  const [settled, setSettled] = useState<Settled | undefined>();
  const [revoking, setRevoking] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | undefined>();
  const [failure, setFailure] = useState<string | undefined>();

  const key = `${projectId}\n${revision}\n${reads}`;
  useEffect(() => {
    let current = true;
    void listProjectGrants(projectId).then(
      (grants) => {
        if (current) setSettled({ key, grants });
      },
      (error: unknown) => {
        if (current) {
          setSettled({
            key,
            failure: asErrorEnvelope(error)?.message ?? "Could not read the standing permissions.",
          });
        }
      },
    );
    return () => {
      current = false;
    };
  }, [key, projectId]);

  const revoke = async (grant: ProjectGrant): Promise<void> => {
    setBusy(true);
    setFailure(undefined);
    setNote(undefined);
    try {
      await revokeProjectGrant(grant.grantId);
      setRevoking(undefined);
      setNote(`Revoked. ${grant.toolId} will be asked about again.`);
      setReads((count) => count + 1);
    } catch (error: unknown) {
      setFailure(asErrorEnvelope(error)?.message ?? "Could not revoke the permission.");
    } finally {
      setBusy(false);
    }
  };

  const grants = settled?.grants;

  return (
    <section className="grants" aria-labelledby="grants-heading">
      <h2 id="grants-heading">Standing permissions</h2>
      {grants !== undefined && grants.length > 0 && (
        // Held in memory, not in the profile, and nothing else says so.
        <p className="grant-meta">
          These last until Tramontana quits; after that, each request is asked about again.
        </p>
      )}

      {settled?.failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {settled.failure}
        </p>
      )}
      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}
      {note !== undefined && (
        <p className="notice" role="status">
          {note}
        </p>
      )}

      {grants === undefined ? null : grants.length === 0 ? (
        <p className="inline-empty">
          None. Every request that needs a decision is asked about each time.
        </p>
      ) : (
        <ul className="grant-list" aria-label="Standing permissions">
          {grants.map((grant) => (
            <li key={grant.grantId}>
              <p>
                <code>{grant.toolId}</code> may use {grant.capability}, up to{" "}
                {CEILING_TEXT[grant.riskCeiling]}.
              </p>
              <p className="grant-meta">Granted {grant.grantedAt.slice(0, 10)}</p>
              {revoking === grant.grantId ? (
                <div className="grant-actions">
                  <span>Revoke it? Its next request will be asked about.</span>
                  <button
                    type="button"
                    className="button button--secondary"
                    disabled={busy}
                    onClick={() => void revoke(grant)}
                  >
                    Revoke
                  </button>
                  <button
                    type="button"
                    className="button button--secondary"
                    onClick={() => {
                      setRevoking(undefined);
                    }}
                  >
                    Keep
                  </button>
                </div>
              ) : (
                <button
                  type="button"
                  className="button button--secondary"
                  disabled={busy}
                  onClick={() => {
                    setRevoking(grant.grantId);
                  }}
                >
                  Revoke…
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
