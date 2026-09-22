import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ApprovalDecisionKind,
  ApprovalRequest,
  RiskClass,
  SideEffect,
} from "@tramontana/shared";

import { asErrorEnvelope, resolveApproval } from "./core.js";

/**
 * The approval card.
 *
 * A tool call that needs a person's decision stops until it gets one. This
 * shows what is being asked for and offers exactly the answers the core says
 * are eligible — never one it would refuse to honor
 * (`SECURITY_AND_PERMISSIONS.md` §4). It decides nothing itself: the labels
 * are presentation, and the decision travels back by identifier.
 *
 * Focus moves here when the question appears, because a question that only
 * changes the screen somewhere below the composer is a question a keyboard or
 * screen-reader user will not know was asked. Focus returns to where it was
 * once the question is answered.
 */

/** What each answer offers, in the words of what is being decided. */
const DECISION_TEXT: Record<ApprovalDecisionKind, string> = {
  allowOnce: "Allow once",
  allowProject: "Allow in this project",
  allowSite: "Allow this site for this task",
  deny: "Deny",
};

/** How much a request could change, as words rather than a color. */
const RISK_TEXT: Record<RiskClass, string> = {
  read: "Reads only",
  modify: "Changes files",
  dangerous: "Runs a command",
};

/**
 * The browser's risk in its own words: its risk classes say how far an
 * action reaches, and "runs a command" is not what a click does (ADR 0047).
 */
const BROWSER_RISK_TEXT: Record<RiskClass, string> = {
  read: "Reads a web page",
  modify: "Acts on a web page",
  dangerous: "Acts on a web page",
};

/** How much a request could change, for this request. */
function riskText(request: ApprovalRequest): string {
  return request.capability.startsWith("browser.")
    ? BROWSER_RISK_TEXT[request.riskClass]
    : RISK_TEXT[request.riskClass];
}

/** What would be left behind by running it. */
const EFFECT_TEXT: Record<SideEffect, string> = {
  none: "Nothing outside Tramontana changes.",
  localReversible: "A local change that can be undone.",
  localIrreversible: "A local change that cannot be undone.",
  external: "An effect outside this device.",
};

interface ApprovalCardProps {
  /** The question at the front of the queue. */
  readonly request: ApprovalRequest;
  /** How many further questions are waiting behind it. */
  readonly waiting: number;
  /** Called once this question has been answered, or can no longer be. */
  readonly onAnswered: (approvalId: string) => void;
}

export function ApprovalCard({
  request,
  waiting,
  onAnswered,
}: ApprovalCardProps): React.JSX.Element {
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | undefined>();
  const card = useRef<HTMLElement>(null);

  // Captured on the way in and restored on the way out, so answering returns
  // the keyboard to whatever the person was doing. Keyed by approval id: a
  // second question replaces the first without stealing the restore target.
  useEffect(() => {
    const previous = document.activeElement;
    card.current?.focus();
    return () => {
      if (previous instanceof HTMLElement && document.contains(previous)) previous.focus();
    };
  }, [request.approvalId]);

  const answer = useCallback(
    async (decision: ApprovalDecisionKind): Promise<void> => {
      setBusy(true);
      setFailure(undefined);
      try {
        await resolveApproval(request.approvalId, decision);
        onAnswered(request.approvalId);
      } catch (error: unknown) {
        // A question that is no longer waiting has already been answered or
        // has timed out, and the task has moved on. Saying so and clearing the
        // card is more honest than leaving buttons that do nothing.
        const envelope = asErrorEnvelope(error);
        if (envelope?.code === "notFound") {
          onAnswered(request.approvalId);
          return;
        }
        setFailure(envelope?.message ?? "Could not deliver that decision.");
      } finally {
        setBusy(false);
      }
    },
    [onAnswered, request.approvalId],
  );

  return (
    <section
      className="approval"
      ref={card}
      tabIndex={-1}
      aria-labelledby="approval-heading"
      aria-describedby="approval-summary"
    >
      <div className="section-heading-row">
        <h2 id="approval-heading">Approval needed</h2>
        <code>{request.toolId}</code>
      </div>

      <p id="approval-summary" className="approval-summary">
        {request.summary}
      </p>

      <dl className="approval-facts">
        <dt>Risk</dt>
        <dd data-risk={request.riskClass}>{riskText(request)}</dd>

        <dt>Effect</dt>
        <dd>{EFFECT_TEXT[request.sideEffect]}</dd>

        {request.details.map((detail) => (
          <div key={`${detail.label}:${detail.value}`} className="approval-detail">
            <dt>{detail.label}</dt>
            <dd>{detail.value}</dd>
          </div>
        ))}
      </dl>

      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}

      <div className="approval-actions">
        {request.eligibleDecisions.map((decision) => (
          <button
            key={decision}
            type="button"
            className={decision === "deny" ? "button button--secondary" : "button button--primary"}
            disabled={busy}
            onClick={() => void answer(decision)}
          >
            {DECISION_TEXT[decision]}
          </button>
        ))}
      </div>

      {request.eligibleDecisions.includes("allowSite") && (
        <p className="approval-note">
          “{DECISION_TEXT.allowSite}” lets this task open, read, click and type on this site without
          asking again. Submitting a form is still asked about every time.
        </p>
      )}

      {request.eligibleDecisions.includes("allowProject") && (
        <p className="approval-note">
          “{DECISION_TEXT.allowProject}” lets later requests like this one through without asking,
          until Tramontana quits.
        </p>
      )}

      <p className="approval-note">
        {waiting > 0
          ? `${waiting} more ${waiting === 1 ? "question is" : "questions are"} waiting behind this one.`
          : "The task is stopped until this is answered."}
      </p>
    </section>
  );
}
