import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ActivityCards } from "./ActivityCards.js";
import type { ActivityCard } from "./cards.js";

const cards: ActivityCard[] = [
  {
    kind: "routing",
    key: "event-1",
    sequence: 1,
    model: "gpt-oss",
    location: "cloud · openai",
    mode: "cloudPreferred",
    leavesDevice: true,
    reasons: ["cloudPreferred"],
    release:
      "9 byte(s) of project context left the device to gpt-oss (openai) — release.approvedOnce",
  },
  {
    kind: "tool",
    key: "event-2",
    sequence: 2,
    toolId: "builtin.fs.patch",
    riskClass: "modify",
    arguments: "1 argument(s): path",
    status: "refused",
    decision: "refused (permission.denied)",
  },
  { kind: "failed", key: "event-3", sequence: 3, detail: "providerUnavailable: no model" },
];

describe("activity cards", () => {
  it("says each state in words, card by card, in the order it happened", () => {
    render(<ActivityCards cards={cards} />);
    const items = within(screen.getByRole("list", { name: "Task activity" })).getAllByRole(
      "listitem",
    );

    expect(items).toHaveLength(3);
    expect(items[0]).toHaveTextContent("Leaves this device");
    expect(items[0]).toHaveTextContent("Prefer cloud");
    expect(items[0]).toHaveTextContent("release.approvedOnce");
    expect(items[1]).toHaveTextContent("Refused");
    expect(items[1]).toHaveTextContent("changes files");
    expect(items[1]).toHaveTextContent("refused (permission.denied)");
    expect(items[2]).toHaveTextContent("Failed");
    expect(items[2]).toHaveTextContent("providerUnavailable: no model");
    expect(items[2]).toHaveTextContent("event 3");
  });

  it("says why there is nothing yet, rather than showing an empty list", () => {
    render(<ActivityCards cards={[]} />);
    expect(screen.queryByRole("list", { name: "Task activity" })).not.toBeInTheDocument();
    expect(screen.getByText(/still getting ready/)).toBeInTheDocument();
  });
});
