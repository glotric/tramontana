import type { ProjectRecord, TaskRecord } from "@tramontana/shared";

/**
 * What the Agent home's activity card says, counted from the task records
 * in this profile. Nothing here is estimated: every number is a count of
 * records, and days are the computer's local days.
 */

export type ActivityRange = "all" | "30d" | "7d";

export interface ActivitySummary {
  readonly tasks: number;
  readonly completed: number;
  /** Failed, cancelled or interrupted. */
  readonly stopped: number;
  /** Local days with at least one task. */
  readonly activeDays: number;
  /** The local hour (0–23) most tasks started in, if there were any. */
  readonly busiestHour: number | undefined;
  /** Projects with at least one task. */
  readonly projects: number;
}

/** One day on the heatmap. */
export interface ActivityDay {
  readonly key: string;
  readonly date: Date;
  readonly count: number;
}

const STOPPED = new Set(["failed", "cancelled", "interrupted"]);

/** A local calendar day as `YYYY-MM-DD`. */
export function dayKey(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

function startOfDay(date: Date): Date {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate());
}

/** The tasks created within the range, counting today as day one. */
export function withinRange(
  tasks: readonly TaskRecord[],
  range: ActivityRange,
  now: Date,
): readonly TaskRecord[] {
  if (range === "all") return tasks;
  const days = range === "7d" ? 7 : 30;
  // Counted in calendar days, not in milliseconds: across a clock change
  // a day is 23 or 25 hours, and the cut-off would move by one.
  const from = new Date(now.getFullYear(), now.getMonth(), now.getDate() - (days - 1)).getTime();
  return tasks.filter((task) => {
    const created = Date.parse(task.createdAt);
    return !Number.isNaN(created) && created >= from;
  });
}

export function summarize(tasks: readonly TaskRecord[]): ActivitySummary {
  const days = new Set<string>();
  const hours = new Array<number>(24).fill(0);
  const projects = new Set<string>();
  let completed = 0;
  let stopped = 0;
  for (const task of tasks) {
    if (task.state === "completed") completed += 1;
    if (STOPPED.has(task.state)) stopped += 1;
    if (task.projectId !== undefined) projects.add(task.projectId);
    const created = new Date(task.createdAt);
    if (Number.isNaN(created.getTime())) continue;
    days.add(dayKey(created));
    const hour = created.getHours();
    hours[hour] = (hours[hour] ?? 0) + 1;
  }
  const most = Math.max(...hours);
  return {
    tasks: tasks.length,
    completed,
    stopped,
    activeDays: days.size,
    busiestHour: most === 0 ? undefined : hours.indexOf(most),
    projects: projects.size,
  };
}

/**
 * Tasks per local day for the last `weeks` weeks, as columns of seven days
 * from Monday. Days after today are `undefined`, so the grid ends on today.
 */
export function heatmap(
  tasks: readonly TaskRecord[],
  now: Date,
  weeks = 26,
): readonly (readonly (ActivityDay | undefined)[])[] {
  const counts = new Map<string, number>();
  for (const task of tasks) {
    const created = new Date(task.createdAt);
    if (Number.isNaN(created.getTime())) continue;
    const key = dayKey(created);
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  const today = startOfDay(now);
  // Monday is 0, Sunday 6.
  const weekday = (today.getDay() + 6) % 7;
  const first = new Date(today.getFullYear(), today.getMonth(), today.getDate() - weekday);
  first.setDate(first.getDate() - (weeks - 1) * 7);
  const columns: (ActivityDay | undefined)[][] = [];
  for (let week = 0; week < weeks; week += 1) {
    const column: (ActivityDay | undefined)[] = [];
    for (let day = 0; day < 7; day += 1) {
      // Built from parts, not by adding milliseconds, so a clock change
      // between two days never skips or repeats one.
      const date = new Date(
        first.getFullYear(),
        first.getMonth(),
        first.getDate() + week * 7 + day,
      );
      if (date.getTime() > today.getTime()) {
        column.push(undefined);
        continue;
      }
      const key = dayKey(date);
      column.push({ key, date, count: counts.get(key) ?? 0 });
    }
    columns.push(column);
  }
  return columns;
}

/** How dark a heatmap cell is: 0 for none, then four steps up to the busiest day. */
export function intensity(count: number, busiest: number): 0 | 1 | 2 | 3 | 4 {
  if (count <= 0 || busiest <= 0) return 0;
  const share = count / busiest;
  if (share > 0.75) return 4;
  if (share > 0.5) return 3;
  if (share > 0.25) return 2;
  return 1;
}

/** Tasks per project, busiest first; tasks without a project are left out. */
export function perProject(
  tasks: readonly TaskRecord[],
  projects: readonly ProjectRecord[],
): readonly { readonly projectId: string; readonly name: string; readonly tasks: number }[] {
  const counts = new Map<string, number>();
  for (const task of tasks) {
    if (task.projectId === undefined) continue;
    counts.set(task.projectId, (counts.get(task.projectId) ?? 0) + 1);
  }
  return [...counts]
    .map(([projectId, count]) => ({
      projectId,
      // A project removed since keeps its count under a plain label.
      name: projects.find((project) => project.projectId === projectId)?.name ?? "Removed project",
      tasks: count,
    }))
    .sort((left, right) => right.tasks - left.tasks || left.name.localeCompare(right.name));
}
