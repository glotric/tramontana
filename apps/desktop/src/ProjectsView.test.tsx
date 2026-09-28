import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectRecord } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const listConversations = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, listConversations };
});

const { ProjectsView } = await import("./ProjectsView.js");

function project(projectId: string, name: string, root: string): ProjectRecord {
  return {
    schemaVersion: 1,
    projectId,
    name,
    roots: [root],
    createdAt: "2026-09-20T10:00:00.000Z",
  };
}

const projects = [
  project("p1", "Harbour cranes", "/work/cranes"),
  project("p2", "Supplier contracts", "/work/contracts"),
];

beforeEach(() => {
  listConversations.mockReset();
  listConversations.mockImplementation((projectId: string) =>
    Promise.resolve(projectId === "p1" ? [{}, {}, {}] : [{}]),
  );
});

describe("ProjectsView", () => {
  it("shows each project with what the record knows, and opens the one picked", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    render(<ProjectsView projects={projects} onOpen={onOpen} onNew={vi.fn()} opening={false} />);

    const cards = screen.getByRole("list", { name: "Projects" });
    expect(await within(cards).findByText(/3 conversations ·/)).toBeInTheDocument();
    expect(await within(cards).findByText(/1 conversation ·/)).toBeInTheDocument();
    expect(within(cards).getByText("/work/cranes")).toBeInTheDocument();

    await user.click(within(cards).getByRole("button", { name: /Harbour cranes/ }));
    expect(onOpen).toHaveBeenCalledWith(projects[0]);
  });

  it("filters by name or folder, and offers to open a new folder", async () => {
    const user = userEvent.setup();
    const onNew = vi.fn();
    render(<ProjectsView projects={projects} onOpen={vi.fn()} onNew={onNew} opening={false} />);

    await user.type(screen.getByLabelText("Filter projects"), "contracts");
    const cards = screen.getByRole("list", { name: "Projects" });
    expect(within(cards).queryByText("Harbour cranes")).not.toBeInTheDocument();
    expect(within(cards).getByText("Supplier contracts")).toBeInTheDocument();

    await user.clear(screen.getByLabelText("Filter projects"));
    await user.type(screen.getByLabelText("Filter projects"), "nothing");
    expect(screen.getByText("No project matches “nothing”.")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Open a folder" }));
    expect(onNew).toHaveBeenCalled();
  });
});
