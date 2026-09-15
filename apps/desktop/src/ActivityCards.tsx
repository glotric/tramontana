import type { RiskClass } from "@tramontana/shared";

import { MODE_LABEL } from "./cards.js";
import type { ActivityCard, DetailCard, ToolStatus } from "./cards.js";

/**
 * The activity cards, in the order things happened.
 *
 * Every state is said in words — a status, a destination, a label — and the
 * `data-*` attributes only let the stylesheet echo it, so nothing is carried by
 * color alone. Each card names its event's sequence number, so it can be found
 * in the full record below it and in the CLI's replay.
 */

const STATUS_TEXT: Record<ToolStatus, string> = {
  requested: "Asked for",
  refused: "Refused",
  running: "Running",
  completed: "Done",
  failed: "Failed",
};

const RISK_TEXT: Record<RiskClass, string> = {
  read: "reads",
  modify: "changes files",
  dangerous: "runs a command",
};

const DETAIL_LABEL: Record<DetailCard["kind"], string> = {
  context: "Context",
  message: "Model",
  checkpoint: "Checkpoint",
  artifact: "Changes",
  note: "Note",
  completed: "Completed",
  cancelled: "Cancelled",
  failed: "Failed",
};

function CardBody({ card }: { readonly card: ActivityCard }): React.JSX.Element {
  switch (card.kind) {
    case "tool":
      return (
        <>
          <div className="card-head">
            <span className="card-label">Tool</span>
            <code>{card.toolId}</code>
            <span className="card-status" data-status={card.status}>
              {STATUS_TEXT[card.status]}
            </span>
          </div>
          <p>
            {card.arguments} · {RISK_TEXT[card.riskClass]}
          </p>
          {card.decision !== undefined && <p className="card-meta">{card.decision}</p>}
          {card.result !== undefined && <p>{card.result}</p>}
        </>
      );
    case "routing":
      return (
        <>
          <div className="card-head">
            <span className="card-label">Route</span>
            <code>{card.model}</code>
            <span className="card-status" data-leaves={card.leavesDevice}>
              {card.leavesDevice ? "Leaves this device" : "Stays on this device"}
            </span>
          </div>
          <p>
            {card.location} · {MODE_LABEL[card.mode]} · {card.reasons.join(", ")}
          </p>
          {card.release !== undefined && <p className="card-meta">{card.release}</p>}
        </>
      );
    default:
      return (
        <>
          <div className="card-head">
            <span className="card-label">{DETAIL_LABEL[card.kind]}</span>
          </div>
          <p>{card.detail}</p>
        </>
      );
  }
}

export function ActivityCards({
  cards,
}: {
  readonly cards: readonly ActivityCard[];
}): React.JSX.Element {
  if (cards.length === 0) {
    return <p className="inline-empty">Nothing to show yet: the task is still getting ready.</p>;
  }
  return (
    <ol className="cards" aria-label="Task activity">
      {cards.map((card) => (
        <li key={card.key} className="card" data-kind={card.kind}>
          <CardBody card={card} />
          <span className="card-sequence">event {card.sequence}</span>
        </li>
      ))}
    </ol>
  );
}
