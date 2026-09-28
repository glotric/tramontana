import type { Cadence, LocalDateTime, Weekday } from "@tramontana/shared";

/** How schedules are described, in the design's sentences (ADR 0063). */

export const WEEKDAYS: readonly Weekday[] = [
  "monday",
  "tuesday",
  "wednesday",
  "thursday",
  "friday",
  "saturday",
  "sunday",
];

const DAY_NAME: Record<Weekday, string> = {
  monday: "Monday",
  tuesday: "Tuesday",
  wednesday: "Wednesday",
  thursday: "Thursday",
  friday: "Friday",
  saturday: "Saturday",
  sunday: "Sunday",
};

/** A time as a 24-hour clock shows it. */
export function clockTime(hour: number, minute: number): string {
  return `${String(hour)}:${String(minute).padStart(2, "0")}`;
}

/** "Every weekday at 8:00", "Fridays at 15:00". */
export function describeCadence(cadence: Cadence): string {
  const at = clockTime(cadence.hour, cadence.minute);
  switch (cadence.kind) {
    case "daily":
      return `Every day at ${at}`;
    case "weekdays":
      return `Every weekday at ${at}`;
    case "weekly":
      return `${DAY_NAME[cadence.weekday]}s at ${at}`;
  }
}

function dayNumber(date: LocalDateTime): number {
  return Date.UTC(date.year, date.month - 1, date.day) / 86_400_000;
}

/** "today at 8:00", "tomorrow at 8:00", or "Mon 5 Oct at 8:00". */
export function describeNext(next: LocalDateTime, today: Date): string {
  const at = clockTime(next.hour, next.minute);
  const days =
    dayNumber(next) - Date.UTC(today.getFullYear(), today.getMonth(), today.getDate()) / 86_400_000;
  if (days === 0) return `today at ${at}`;
  if (days === 1) return `tomorrow at ${at}`;
  const date = new Date(next.year, next.month - 1, next.day);
  const day = date.toLocaleDateString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
  });
  return `${day} at ${at}`;
}

/** The cadence a form's choices make. */
export function cadenceFrom(every: string, time: string): Cadence | undefined {
  const match = /^(\d{1,2}):(\d{2})$/.exec(time);
  if (match === null) return undefined;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  if (hour > 23 || minute > 59) return undefined;
  if (every === "day") return { kind: "daily", hour, minute };
  if (every === "weekday") return { kind: "weekdays", hour, minute };
  const weekday = WEEKDAYS.find((day) => day === every);
  return weekday === undefined ? undefined : { kind: "weekly", weekday, hour, minute };
}

export { DAY_NAME };
