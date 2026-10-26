import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectRecord } from "@tramontana/shared";

const listConversations = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  listConversations,
}));

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
  listConversations.mockImplementation((projectId: string) =>
    Promise.resolve(projectId === "p1" ? [{}, {}, {}] : [{}]),
  );
});

describe("ProjectsView", () => {
  it("shows each project with what the record knows, and opens the one picked", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    render(
      <ProjectsView
        projects={projects}
        onOpen={onOpen}
        onNew={vi.fn()}
        onDelete={vi.fn()}
        opening={false}
      />,
    );

    const cards = screen.getByRole("list", { name: "Projects" });
    expect(await within(cards).findByText(/3 conversations ·/)).toBeInTheDocument();
    expect(await within(cards).findByText(/1 conversation ·/)).toBeInTheDocument();
    expect(within(cards).getByText("/work/cranes")).toBeInTheDocument();

    await user.click(within(cards).getByRole("button", { name: /^Harbour cranes/ }));
    expect(onOpen).toHaveBeenCalledWith(projects[0]);
  });

  it("filters by name or folder, and offers to open a new folder", async () => {
    const user = userEvent.setup();
    const onNew = vi.fn();
    render(
      <ProjectsView
        projects={projects}
        onOpen={vi.fn()}
        onNew={onNew}
        onDelete={vi.fn()}
        opening={false}
      />,
    );

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

  it("removes a project only after saying its files are not touched", async () => {
    const user = userEvent.setup();
    const onDelete = vi.fn(() => Promise.resolve());
    render(
      <ProjectsView
        projects={projects}
        onOpen={vi.fn()}
        onNew={vi.fn()}
        onDelete={onDelete}
        opening={false}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Remove Harbour cranes from Tramontana" }));
    const confirm = screen.getByRole("alertdialog", { name: "Remove Harbour cranes?" });
    expect(confirm).toHaveTextContent("The folder and its files are not touched.");
    await user.click(within(confirm).getByRole("button", { name: "Keep" }));
    expect(onDelete).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Remove Harbour cranes from Tramontana" }));
    await user.click(screen.getByRole("button", { name: "Remove" }));
    expect(onDelete).toHaveBeenCalledWith(projects[0]);
  });

  it("says why a project could not be removed", async () => {
    const user = userEvent.setup();
    const onDelete = vi.fn().mockRejectedValue({
      schemaVersion: 1,
      code: "conflict",
      message: "Stop the project's running task before removing it",
      retryable: true,
      correlationId: "c",
    });
    render(
      <ProjectsView
        projects={projects}
        onOpen={vi.fn()}
        onNew={vi.fn()}
        onDelete={onDelete}
        opening={false}
      />,
    );
    await user.click(
      screen.getByRole("button", { name: "Remove Supplier contracts from Tramontana" }),
    );
    await user.click(screen.getByRole("button", { name: "Remove" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Stop the project's running task");
  });
});
