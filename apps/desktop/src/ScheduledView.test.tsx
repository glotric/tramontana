import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectRecord, ScheduleView } from "@tramontana/shared";

import { cadenceFrom, describeCadence, describeNext } from "./schedule.js";

const listSchedules = vi.hoisted(() => vi.fn());
const createSchedule = vi.hoisted(() => vi.fn());
const setScheduleEnabled = vi.hoisted(() => vi.fn());
const deleteSchedule = vi.hoisted(() => vi.fn());
const runScheduleNow = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  listSchedules,
  createSchedule,
  setScheduleEnabled,
  deleteSchedule,
  runScheduleNow,
}));

const { ScheduledView } = await import("./ScheduledView.js");

const project: ProjectRecord = {
  schemaVersion: 1,
  projectId: "p1",
  name: "Harbour cranes",
  roots: ["/work/cranes"],
  createdAt: "2026-09-01T00:00:00Z",
};

const morning: ScheduleView = {
  schedule: {
    schemaVersion: 1,
    scheduleId: "s1",
    title: "Morning plan",
    goal: "Plan my day.",
    conversationId: "c1",
    cadence: { kind: "weekdays", hour: 8, minute: 0 },
    routingMode: "localOnly",
    enabled: true,
    createdAt: "2026-09-28T00:00:00Z",
    createdLocal: { year: 2026, month: 9, day: 28, hour: 7, minute: 0 },
  },
  nextRun: { year: 2026, month: 9, day: 29, hour: 8, minute: 0 },
};

beforeEach(() => {
  listSchedules.mockResolvedValue([morning]);
});

describe("schedule wording", () => {
  it("says a cadence and its next run the way the design does", () => {
    expect(describeCadence({ kind: "weekdays", hour: 8, minute: 0 })).toBe("Every weekday at 8:00");
    expect(describeCadence({ kind: "weekly", weekday: "friday", hour: 15, minute: 5 })).toBe(
      "Fridays at 15:05",
    );
    const today = new Date(2026, 8, 28, 7, 0);
    expect(describeNext({ year: 2026, month: 9, day: 28, hour: 8, minute: 0 }, today)).toBe(
      "today at 8:00",
    );
    expect(describeNext({ year: 2026, month: 9, day: 29, hour: 8, minute: 0 }, today)).toBe(
      "tomorrow at 8:00",
    );
  });

  it("makes a cadence only from a real time", () => {
    expect(cadenceFrom("friday", "15:00")).toEqual({
      kind: "weekly",
      weekday: "friday",
      hour: 15,
      minute: 0,
    });
    expect(cadenceFrom("day", "7:30")).toEqual({ kind: "daily", hour: 7, minute: 30 });
    expect(cadenceFrom("day", "24:00")).toBeUndefined();
    expect(cadenceFrom("sometimes", "08:00")).toBeUndefined();
  });
});

describe("ScheduledView", () => {
  it("lists schedules with when they run next, and says nothing runs while closed", async () => {
    const onOpen = vi.fn();
    const user = userEvent.setup();
    render(<ScheduledView projects={[project]} onOpen={onOpen} />);

    const list = await screen.findByRole("list", { name: "Scheduled tasks" });
    expect(list).toHaveTextContent("Morning plan");
    expect(list).toHaveTextContent("Every weekday at 8:00 · A chat · Next:");
    expect(screen.getByRole("note")).toHaveTextContent("Nothing runs while Tramontana is closed");

    await user.click(within(list).getByRole("button", { name: "Open" }));
    expect(onOpen).toHaveBeenCalledWith(morning.schedule);
  });

  it("creates a schedule from an idea, in the project chosen", async () => {
    const user = userEvent.setup();
    createSchedule.mockResolvedValue(morning);
    render(<ScheduledView projects={[project]} onOpen={vi.fn()} />);

    await user.click(await screen.findByRole("button", { name: /Weekly project summary/ }));
    const form = screen.getByRole("form", { name: "New scheduled task" });
    expect(within(form).getByLabelText("Name")).toHaveValue("Weekly project summary");
    await user.click(within(form).getByRole("button", { name: "Schedule" }));

    await waitFor(() => {
      expect(createSchedule).toHaveBeenCalledWith({
        title: "Weekly project summary",
        goal: "Summarize what changed in this project this week, from its Git history.",
        cadence: { kind: "weekly", weekday: "friday", hour: 15, minute: 0 },
        routingMode: "localOnly",
        projectId: "p1",
      });
    });
  });

  it("pauses, runs now, and deletes only after asking", async () => {
    const user = userEvent.setup();
    setScheduleEnabled.mockResolvedValue(morning.schedule);
    runScheduleNow.mockResolvedValue("task-1");
    deleteSchedule.mockResolvedValue(undefined);
    render(<ScheduledView projects={[]} onOpen={vi.fn()} />);

    await user.click(await screen.findByRole("switch", { name: "Morning plan is on" }));
    expect(setScheduleEnabled).toHaveBeenCalledWith("s1", false);

    await user.click(screen.getByRole("button", { name: "Run now" }));
    expect(await screen.findByRole("status")).toHaveTextContent("“Morning plan” is running now.");

    await user.click(screen.getByRole("button", { name: "Delete Morning plan" }));
    expect(deleteSchedule).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Delete" }));
    expect(deleteSchedule).toHaveBeenCalledWith("s1");
  });

  it("does not offer a project idea when there is no project", async () => {
    render(<ScheduledView projects={[]} onOpen={vi.fn()} />);
    expect(await screen.findByRole("button", { name: /Weekly project summary/ })).toBeDisabled();
  });
});
