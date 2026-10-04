import { describe, expect, it } from "vitest";
import type { ProjectRecord, TaskRecord } from "@tramontana/shared";

import {
  dayKey,
  heatmap,
  intensity,
  perProject,
  summarize,
  withinRange,
} from "./activity-overview.js";

/** A task created at a local time, so the tests mean the same in any zone. */
function task(
  id: string,
  at: Date,
  state: TaskRecord["state"] = "completed",
  /** `null` for a task in no project. */
  projectId: string | null = "p1",
): TaskRecord {
  return {
    schemaVersion: 1,
    taskId: id,
    goalSummary: id,
    state,
    lastSequence: 1,
    createdAt: at.toISOString(),
    updatedAt: at.toISOString(),
    ...(projectId === null ? {} : { projectId }),
  };
}

const project = (projectId: string, name: string): ProjectRecord => ({
  schemaVersion: 1,
  projectId,
  name,
  roots: [`/work/${name}`],
  createdAt: "2026-01-01T00:00:00.000Z",
});

// Sunday 4 October 2026, mid-afternoon, local time.
const now = new Date(2026, 9, 4, 15, 30);

describe("the activity card's counting", () => {
  it("counts tasks, outcomes, active days, the busiest hour and projects", () => {
    const tasks = [
      task("a", new Date(2026, 9, 4, 10, 5)),
      task("b", new Date(2026, 9, 4, 10, 40), "failed"),
      task("c", new Date(2026, 9, 3, 23, 59), "cancelled", "p2"),
      task("d", new Date(2026, 9, 1, 9, 0), "interrupted"),
      task("e", new Date(2026, 9, 1, 10, 0), "running", null),
    ];
    expect(summarize(tasks)).toEqual({
      tasks: 5,
      completed: 1,
      stopped: 3,
      activeDays: 3,
      busiestHour: 10,
      projects: 2,
    });
  });

  it("has no busiest hour without tasks", () => {
    expect(summarize([])).toEqual({
      tasks: 0,
      completed: 0,
      stopped: 0,
      activeDays: 0,
      busiestHour: undefined,
      projects: 0,
    });
  });

  it("keeps the last 7 or 30 local days, counting today", () => {
    const tasks = [
      task("today", new Date(2026, 9, 4, 0, 1)),
      task("six days ago", new Date(2026, 8, 28, 0, 0)),
      task("seven days ago", new Date(2026, 8, 27, 23, 59)),
      task("29 days ago", new Date(2026, 8, 5, 8, 0)),
      task("30 days ago", new Date(2026, 8, 4, 8, 0)),
    ];
    const ids = (range: "all" | "7d" | "30d"): string[] =>
      withinRange(tasks, range, now).map((t) => t.taskId);
    expect(ids("7d")).toEqual(["today", "six days ago"]);
    expect(ids("30d")).toEqual(["today", "six days ago", "seven days ago", "29 days ago"]);
    expect(ids("all")).toHaveLength(5);
  });

  it("buckets tasks by local day, in weeks from Monday that end today", () => {
    const tasks = [
      task("a", new Date(2026, 9, 4, 9, 0)),
      task("b", new Date(2026, 9, 4, 18, 0)),
      task("c", new Date(2026, 8, 29, 12, 0)),
    ];
    const weeks = heatmap(tasks, now, 2);
    expect(weeks).toHaveLength(2);
    // The first column starts on the Monday a week before this one.
    expect(weeks[0]?.[0]?.key).toBe("2026-09-21");
    const thisWeek = weeks[1] ?? [];
    expect(thisWeek[0]?.key).toBe("2026-09-28");
    expect(thisWeek[1]?.count).toBe(1);
    // Sunday is today, the grid's last day.
    expect(thisWeek[6]?.key).toBe(dayKey(now));
    expect(thisWeek[6]?.count).toBe(2);
  });

  it("leaves days still to come empty", () => {
    const wednesday = new Date(2026, 9, 7, 12, 0);
    const lastWeek = heatmap([], wednesday, 1)[0] ?? [];
    expect(lastWeek.slice(0, 3).every((day) => day !== undefined)).toBe(true);
    expect(lastWeek.slice(3).every((day) => day === undefined)).toBe(true);
  });

  it("shades a day by its share of the busiest day", () => {
    expect([0, 1, 3, 6, 8].map((count) => intensity(count, 8))).toEqual([0, 1, 2, 3, 4]);
    expect(intensity(3, 0)).toBe(0);
  });

  it("counts each project's tasks, busiest first, and names a removed one plainly", () => {
    const tasks = [
      task("a", now, "completed", "p1"),
      task("b", now, "completed", "p2"),
      task("c", now, "completed", "p2"),
      task("d", now, "completed", "gone"),
      task("e", now, "completed", null),
    ];
    expect(perProject(tasks, [project("p1", "harbor"), project("p2", "crane")])).toEqual([
      { projectId: "p2", name: "crane", tasks: 2 },
      { projectId: "p1", name: "harbor", tasks: 1 },
      { projectId: "gone", name: "Removed project", tasks: 1 },
    ]);
  });
});
