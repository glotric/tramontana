import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const projectChanges = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, projectChanges };
});

const { ChangesPanel } = await import("./ChangesPanel.js");

const DIFF = [
  "diff --git a/greeting.txt b/greeting.txt",
  "--- a/greeting.txt",
  "+++ b/greeting.txt",
  "@@ -1 +1 @@",
  "-hello",
  "+hello, world",
].join("\n");

describe("the Changes panel", () => {
  beforeEach(() => {
    projectChanges.mockReset();
    projectChanges.mockResolvedValue({
      schemaVersion: 1,
      status: "## main\n?? notes.txt\n",
      diff: DIFF,
      truncated: false,
    });
  });

  it("shows the status, and each diff line with Git's own mark", async () => {
    render(<ChangesPanel projectId="project-1" revision={0} />);

    expect(await screen.findByLabelText("Git status")).toHaveTextContent("?? notes.txt");
    const removed = screen.getByText("-hello");
    const added = screen.getByText("+hello, world");
    expect(removed).toHaveAttribute("data-line", "removed");
    expect(added).toHaveAttribute("data-line", "added");
    expect(screen.getByText("@@ -1 +1 @@")).toHaveAttribute("data-line", "hunk");
    expect(projectChanges).toHaveBeenCalledWith("project-1");
  });

  it("reads again when asked, and when the shell says a task ended", async () => {
    const user = userEvent.setup();
    const { rerender } = render(<ChangesPanel projectId="project-1" revision={0} />);
    await screen.findByLabelText("Git status");

    await user.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => {
      expect(projectChanges).toHaveBeenCalledTimes(2);
    });

    rerender(<ChangesPanel projectId="project-1" revision={1} />);
    await waitFor(() => {
      expect(projectChanges).toHaveBeenCalledTimes(3);
    });
  });

  it("says so when there is no diff, when the output was cut short, and when the read failed", async () => {
    projectChanges.mockResolvedValueOnce({
      schemaVersion: 1,
      status: "## main\n",
      diff: "",
      truncated: true,
    });
    const user = userEvent.setup();
    render(<ChangesPanel projectId="project-1" revision={0} />);

    expect(await screen.findByText(/No changes to tracked files/)).toBeInTheDocument();
    expect(screen.getByRole("note")).toHaveTextContent("cut short");

    projectChanges.mockRejectedValueOnce({
      code: "toolFailed",
      message: "git exited unsuccessfully (128)",
      retryable: false,
    });
    await user.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("git exited unsuccessfully (128)");
    expect(screen.queryByLabelText("Git status")).not.toBeInTheDocument();
  });
});
