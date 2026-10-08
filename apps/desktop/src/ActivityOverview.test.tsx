import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { ProjectRecord, TaskRecord } from "@tramontana/shared";

import { ActivityOverview } from "./ActivityOverview.js";

const now = new Date(2026, 9, 4, 15, 0);

function task(id: string, daysAgo: number, projectId: string): TaskRecord {
  const at = new Date(2026, 9, 4 - daysAgo, 11, 0);
  return {
    schemaVersion: 1,
    taskId: id,
    goalSummary: id,
    state: "completed",
    lastSequence: 1,
    projectId,
    createdAt: at.toISOString(),
    updatedAt: at.toISOString(),
  };
}

const projects: ProjectRecord[] = [
  { schemaVersion: 1, projectId: "p1", name: "harbor", roots: ["/h"], createdAt: "2026-01-01" },
  { schemaVersion: 1, projectId: "p2", name: "crane", roots: ["/c"], createdAt: "2026-01-01" },
];

/** A tile's number, found by its label among the tiles. */
function tile(label: string): string {
  const term = [...document.querySelectorAll(".overview-tiles dt")].find(
    (dt) => dt.textContent === label,
  );
  return term?.nextElementSibling?.textContent ?? "";
}

describe("ActivityOverview", () => {
  it("says what the tasks add up to, for the period chosen", async () => {
    const user = userEvent.setup();
    const tasks = [task("a", 0, "p1"), task("b", 2, "p1"), task("c", 40, "p2")];
    render(<ActivityOverview tasks={tasks} projects={projects} now={now} />);

    expect(tile("Tasks")).toBe("3");
    expect(tile("Active days")).toBe("3");
    expect(tile("Projects with tasks")).toBe("2");
    expect(screen.getByRole("img", { name: /Tasks per day over the last 26 weeks/ })).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "7d" }));
    expect(screen.getByRole("button", { name: "7d" })).toHaveAttribute("aria-pressed", "true");
    expect(tile("Tasks")).toBe("2");
    expect(tile("Projects with tasks")).toBe("1");

    await user.click(screen.getByRole("button", { name: "Projects" }));
    const list = screen.getByRole("list", { name: "Tasks per project" });
    expect(
      within(list)
        .getAllByRole("listitem")
        .map((item) => item.textContent),
    ).toEqual(["harbor2"]);
  });

  it("says plainly when there is nothing to count yet, or still reading", () => {
    const { rerender } = render(<ActivityOverview tasks={undefined} projects={[]} now={now} />);
    expect(screen.getByText("Reading your tasks…")).toBeTruthy();
    rerender(<ActivityOverview tasks={[]} projects={[]} now={now} />);
    expect(screen.getByText("No tasks yet. Your activity will appear here.")).toBeTruthy();
    expect(screen.queryByRole("img")).toBeNull();
  });
});
