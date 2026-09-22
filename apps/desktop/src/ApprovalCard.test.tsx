import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ApprovalRequest } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const resolveApproval = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, resolveApproval };
});

const { ApprovalCard } = await import("./ApprovalCard.js");

const request: ApprovalRequest = {
  schemaVersion: 1,
  approvalId: "approval-1",
  taskId: "task-1",
  invocationId: "invocation-1",
  capability: "fs.write",
  toolId: "builtin.fs.patch",
  requestedScope: {
    projectId: "project-1",
    roots: ["/work/tramontana"],
    releasesData: false,
  },
  riskClass: "modify",
  sideEffect: "localReversible",
  summary: "builtin.fs.patch wants to write greeting.txt",
  details: [
    { label: "Path", value: "greeting.txt" },
    { label: "Change", value: "1 line" },
  ],
  eligibleDecisions: ["allowOnce", "deny"],
};

describe("the approval card", () => {
  beforeEach(() => {
    resolveApproval.mockReset();
    resolveApproval.mockResolvedValue(undefined);
  });

  it("shows what is being asked for, in words rather than color alone", () => {
    render(<ApprovalCard request={request} waiting={0} onAnswered={() => undefined} />);

    expect(screen.getByText(request.summary)).toBeInTheDocument();
    expect(screen.getByText("builtin.fs.patch")).toBeInTheDocument();
    expect(screen.getByText("Changes files")).toBeInTheDocument();
    expect(screen.getByText("A local change that can be undone.")).toBeInTheDocument();
    expect(screen.getByText("greeting.txt")).toBeInTheDocument();
    expect(screen.getByText("1 line")).toBeInTheDocument();
    expect(screen.getByText("The task is stopped until this is answered.")).toBeInTheDocument();
  });

  it("offers only the answers the core says are eligible", () => {
    render(<ApprovalCard request={request} waiting={1} onAnswered={() => undefined} />);

    expect(screen.getByRole("button", { name: "Allow once" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Deny" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Allow in this project" })).not.toBeInTheDocument();
    expect(screen.queryByText(/until Tramontana quits/)).not.toBeInTheDocument();
    expect(screen.getByText("1 more question is waiting behind this one.")).toBeInTheDocument();
  });

  it("says how long an answer for the whole project lasts", () => {
    render(
      <ApprovalCard
        request={{ ...request, eligibleDecisions: ["allowOnce", "allowProject", "deny"] }}
        waiting={0}
        onAnswered={() => undefined}
      />,
    );

    expect(screen.getByRole("button", { name: "Allow in this project" })).toBeVisible();
    expect(screen.getByText(/until Tramontana quits/)).toBeVisible();
  });

  it("asks about a web site in the browser's words, and can allow it for the task", async () => {
    const user = userEvent.setup();
    const answered = vi.fn();
    render(
      <ApprovalCard
        request={{
          ...request,
          capability: "browser.navigate",
          toolId: "builtin.browser.open",
          riskClass: "dangerous",
          sideEffect: "external",
          summary: "builtin.browser.open wants to browser.navigate",
          details: [
            { label: "Open", value: "https://example.com/pricing" },
            { label: "Site", value: "https://example.com" },
          ],
          eligibleDecisions: ["allowOnce", "allowSite", "deny"],
        }}
        waiting={0}
        onAnswered={answered}
      />,
    );

    expect(screen.getByText("Acts on a web page")).toBeInTheDocument();
    expect(screen.queryByText("Runs a command")).not.toBeInTheDocument();
    expect(screen.getByText("An effect outside this device.")).toBeInTheDocument();
    expect(screen.getByText("https://example.com/pricing")).toBeInTheDocument();
    expect(screen.getByText(/Submitting a form is still asked about every time/)).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Allow this site for this task" }));
    await waitFor(() => {
      expect(resolveApproval).toHaveBeenCalledWith("approval-1", "allowSite");
    });
  });

  it("takes the focus when it appears and gives it back once answered", async () => {
    const user = userEvent.setup();
    const answered = vi.fn();
    render(<button type="button">Start task</button>);
    const start = screen.getByRole("button", { name: "Start task" });
    start.focus();

    const card = render(<ApprovalCard request={request} waiting={0} onAnswered={answered} />);
    await waitFor(() => {
      expect(document.activeElement).toHaveAttribute("aria-labelledby", "approval-heading");
    });

    await user.click(screen.getByRole("button", { name: "Allow once" }));
    await waitFor(() => {
      expect(answered).toHaveBeenCalledWith("approval-1");
    });

    // Unmounting is what the shell does once a question is answered.
    card.unmount();
    expect(document.activeElement).toBe(start);
  });

  it("can be answered without a pointer", async () => {
    // Focus lands on the card, so the next thing the keyboard reaches is the
    // first answer. A question that stops a task has to be answerable by
    // whoever is asked, not only by whoever has a mouse.
    const user = userEvent.setup();
    const answered = vi.fn();
    render(<ApprovalCard request={request} waiting={0} onAnswered={answered} />);
    await waitFor(() => {
      expect(document.activeElement).toHaveAttribute("aria-labelledby", "approval-heading");
    });

    await user.tab();
    expect(screen.getByRole("button", { name: "Allow once" })).toHaveFocus();
    await user.keyboard("{Enter}");

    await waitFor(() => {
      expect(resolveApproval).toHaveBeenCalledWith("approval-1", "allowOnce");
    });
    expect(answered).toHaveBeenCalledWith("approval-1");
  });

  it("sends the decision by identifier", async () => {
    const user = userEvent.setup();
    const answered = vi.fn();
    render(<ApprovalCard request={request} waiting={0} onAnswered={answered} />);

    await user.click(screen.getByRole("button", { name: "Deny" }));
    await waitFor(() => {
      expect(resolveApproval).toHaveBeenCalledWith("approval-1", "deny");
    });
    expect(answered).toHaveBeenCalledWith("approval-1");
  });

  it("clears a question that is no longer waiting", async () => {
    resolveApproval.mockRejectedValue({
      code: "notFound",
      message: "that approval is no longer waiting for an answer",
      retryable: false,
    });
    const user = userEvent.setup();
    const answered = vi.fn();
    render(<ApprovalCard request={request} waiting={0} onAnswered={answered} />);

    await user.click(screen.getByRole("button", { name: "Allow once" }));
    await waitFor(() => {
      expect(answered).toHaveBeenCalledWith("approval-1");
    });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("keeps the question open when the decision could not be delivered", async () => {
    resolveApproval.mockRejectedValue({
      code: "internal",
      message: "the shell went away",
      retryable: true,
    });
    const user = userEvent.setup();
    const answered = vi.fn();
    render(<ApprovalCard request={request} waiting={0} onAnswered={answered} />);

    await user.click(screen.getByRole("button", { name: "Allow once" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("the shell went away");
    expect(answered).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Allow once" })).toBeEnabled();
  });
});
