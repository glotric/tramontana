import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectGrant } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const listProjectGrants = vi.hoisted(() => vi.fn());
const revokeProjectGrant = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, listProjectGrants, revokeProjectGrant };
});

const { GrantsPanel } = await import("./GrantsPanel.js");

const grant: ProjectGrant = {
  schemaVersion: 1,
  grantId: "grant-1",
  projectId: "project-1",
  capability: "fs.write",
  toolId: "builtin.fs.patch",
  scope: { projectId: "project-1", roots: ["/work/tramontana"], releasesData: false },
  riskCeiling: "modify",
  grantedAt: "2026-09-15T10:00:00.000Z",
};

describe("standing permissions", () => {
  beforeEach(() => {
    listProjectGrants.mockResolvedValue([grant]);
    revokeProjectGrant.mockResolvedValue(undefined);
  });

  it("says what each grant lets a tool do, up to what, and since when", async () => {
    render(<GrantsPanel projectId="project-1" revision={0} />);
    const list = await screen.findByRole("list", { name: "Standing permissions" });
    const item = within(list).getByRole("listitem");
    expect(item).toHaveTextContent("builtin.fs.patch may use fs.write, up to changing files.");
    expect(item).toHaveTextContent("Granted 2026-09-15");
    expect(screen.getByText(/last until Tramontana quits/)).toBeVisible();
    expect(listProjectGrants).toHaveBeenCalledWith("project-1");
  });

  it("asks before revoking, then reads the list again", async () => {
    const user = userEvent.setup();
    render(<GrantsPanel projectId="project-1" revision={0} />);
    await screen.findByRole("list", { name: "Standing permissions" });

    await user.click(screen.getByRole("button", { name: "Revoke…" }));
    expect(revokeProjectGrant).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Keep" }));
    await user.click(screen.getByRole("button", { name: "Revoke…" }));

    listProjectGrants.mockResolvedValue([]);
    await user.click(screen.getByRole("button", { name: "Revoke" }));
    await waitFor(() => {
      expect(revokeProjectGrant).toHaveBeenCalledWith("grant-1");
    });
    expect(await screen.findByRole("status")).toHaveTextContent(
      "builtin.fs.patch will be asked about again",
    );
    expect(await screen.findByText(/Every request that needs a decision is asked/)).toBeVisible();
  });

  it("reads again when the shell says an approval was answered", async () => {
    const { rerender } = render(<GrantsPanel projectId="project-1" revision={0} />);
    await screen.findByRole("list", { name: "Standing permissions" });
    rerender(<GrantsPanel projectId="project-1" revision={1} />);
    await waitFor(() => {
      expect(listProjectGrants).toHaveBeenCalledTimes(2);
    });
  });
});
