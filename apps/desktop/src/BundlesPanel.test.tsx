import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ExportManifest } from "@tramontana/shared";

const exportBundle = vi.hoisted(() => vi.fn());
const inspectBundle = vi.hoisted(() => vi.fn());
const importStagedBundle = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  exportBundle,
  inspectBundle,
  importStagedBundle,
}));

const { BundlesPanel } = await import("./BundlesPanel.js");

const manifest: ExportManifest = {
  schemaVersion: 1,
  bundleMajorVersion: 1,
  bundleMinorVersion: 0,
  createdAt: "2026-01-01T00:00:00.000Z",
  entries: [],
  omissions: [
    { category: "absolutePath", count: 2, reason: "names this machine" },
    { category: "rawToolOutput", count: 2, reason: "can hold file contents" },
    { category: "secret", count: 0, reason: "never exported" },
  ],
  options: { includeAbsolutePaths: false, includeRawToolOutput: false, includeArtifacts: false },
  taskIds: ["task-1"],
};

const counts = {
  projects: 1,
  conversations: 1,
  tasks: 1,
  events: 40,
  checkpoints: 1,
  configurations: 1,
  quarantined: 0,
};

describe("the Bundles panel", () => {
  beforeEach(() => {
    exportBundle.mockResolvedValue({ schemaVersion: 1, directory: "/chosen/bundle", manifest });
    inspectBundle.mockResolvedValue({
      stagingId: "bundle-1",
      inspection: { schemaVersion: 1, manifest, contains: counts },
    });
    importStagedBundle.mockResolvedValue({
      schemaVersion: 1,
      imported: counts,
      executedAnything: false,
    });
  });

  it("exports the project without paths or raw output unless asked, and says what it left out", async () => {
    const user = userEvent.setup();
    render(<BundlesPanel projectId="project-1" taskId={undefined} onImported={() => undefined} />);

    expect(screen.getByRole("button", { name: "Export this task…" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Export project…" }));

    await waitFor(() => {
      expect(exportBundle).toHaveBeenCalledWith(
        { projectId: "project-1" },
        { includeAbsolutePaths: false, includeRawToolOutput: false },
      );
    });
    const result = await screen.findByRole("status");
    expect(result).toHaveTextContent("/chosen/bundle");
    expect(result).toHaveTextContent("Left out: 2 absolute path(s), 2 raw tool output(s).");
    expect(result).not.toHaveTextContent("secret");
  });

  it("exports the task on screen with what the person chose to keep", async () => {
    const user = userEvent.setup();
    render(<BundlesPanel projectId="project-1" taskId="task-1" onImported={() => undefined} />);

    await user.click(screen.getByRole("checkbox", { name: /Keep raw tool output/ }));
    await user.click(screen.getByRole("button", { name: "Export this task…" }));
    await waitFor(() => {
      expect(exportBundle).toHaveBeenCalledWith(
        { taskId: "task-1" },
        { includeAbsolutePaths: false, includeRawToolOutput: true },
      );
    });
  });

  it("shows what a bundle holds before writing it, then imports it by identifier", async () => {
    const onImported = vi.fn();
    const user = userEvent.setup();
    render(<BundlesPanel projectId="project-1" taskId={undefined} onImported={onImported} />);

    await user.click(screen.getByRole("button", { name: "Import a bundle…" }));
    const staged = await screen.findByRole("group", { name: "Bundle to import" });
    expect(staged).toHaveTextContent("1 task(s), 40 event(s)");
    expect(staged).toHaveTextContent("Nothing has been written yet.");
    expect(importStagedBundle).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Import into this profile" }));
    await waitFor(() => {
      expect(importStagedBundle).toHaveBeenCalledWith("bundle-1");
    });
    expect(await screen.findByRole("status")).toHaveTextContent("Nothing in the bundle was run.");
    expect(onImported).toHaveBeenCalledOnce();
  });

  it("offers import alone when no project is open, as in a new profile", () => {
    render(<BundlesPanel projectId={undefined} taskId={undefined} onImported={() => undefined} />);
    expect(screen.getByRole("button", { name: "Import a bundle…" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Export project…" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Export this task…" })).not.toBeInTheDocument();
  });

  it("says a bundle is already in this profile, not which record clashed", async () => {
    const user = userEvent.setup();
    importStagedBundle.mockRejectedValueOnce({
      code: "conflict",
      message: "project project-1 conflicts with an existing record",
      retryable: false,
    });
    render(<BundlesPanel projectId={undefined} taskId={undefined} onImported={() => undefined} />);

    await user.click(screen.getByRole("button", { name: "Import a bundle…" }));
    await screen.findByRole("group", { name: "Bundle to import" });
    await user.click(screen.getByRole("button", { name: "Import into this profile" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("This profile already holds this bundle's records");
    expect(alert).not.toHaveTextContent("project-1");
  });

  it("imports nothing when the person cancels, and says why a bundle was refused", async () => {
    const user = userEvent.setup();
    render(<BundlesPanel projectId="project-1" taskId={undefined} onImported={() => undefined} />);

    await user.click(screen.getByRole("button", { name: "Import a bundle…" }));
    await screen.findByRole("group", { name: "Bundle to import" });
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: "Bundle to import" })).not.toBeInTheDocument();
    expect(importStagedBundle).not.toHaveBeenCalled();

    inspectBundle.mockRejectedValueOnce({
      code: "invalidRequest",
      message: "could not read the bundle: no manifest.json",
      retryable: false,
    });
    await user.click(screen.getByRole("button", { name: "Import a bundle…" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("no manifest.json");
  });
});
