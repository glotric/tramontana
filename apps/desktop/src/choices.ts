import type { RoutingMode } from "@tramontana/shared";

import type { Admission } from "./useComposerOptions.js";

/** Where a task's models may run, in the order the composer offers them. */
export const MODES: readonly RoutingMode[] = ["localOnly", "automatic", "cloudPreferred"];

/** What each mode means for the task about to start. */
export const MODE_TEXT: Record<RoutingMode, string> = {
  localOnly: "Local only: models run on this device, and nothing is sent to a cloud model.",
  automatic:
    "Automatic: a model is chosen for you, and anything sent to a cloud model is asked about first.",
  cloudPreferred:
    "Prefer cloud: a cloud model is chosen when one is available, and each release to it is asked about first.",
};

/** What a task may do beyond reading, as the Access chip says it. */
const ACCESS_SHORT: Record<keyof Admission, string> = {
  writes: "Edit files",
  recipes: "Recipes",
  apps: "Apps",
  browse: "Web",
};

/** The Access chip's words: "Read-only", or what was allowed. */
export function describeAccess(admission: Admission): string {
  const allowed = (Object.keys(ACCESS_SHORT) as (keyof Admission)[]).filter(
    (key) => admission[key],
  );
  return allowed.length === 0 ? "Read-only" : allowed.map((key) => ACCESS_SHORT[key]).join(" · ");
}

/**
 * Say, before the task starts, what the person's choices let it do.
 *
 * Local Only governs where models run, not what a command does: an approved
 * recipe can still reach the network, so choosing both says so (ADR 0028's
 * boundary statement, and the recipe prompt, say the same).
 */
export function describeChoices(
  mode: RoutingMode,
  { writes, recipes, apps, browse }: Admission,
  skills: number,
): string {
  const asks = [
    ...(writes ? ["edit files"] : []),
    ...(recipes ? ["run the project's recipes"] : []),
    ...(apps ? ["use the Apps' reading tools"] : []),
    ...(browse ? ["open websites in a window of its own"] : []),
  ];
  const access =
    asks.length === 0
      ? "Read-only: the model can look at the project but change nothing."
      : `The model may ask to ${asks.join(" and ")}; each one is put to you before it runs.`;
  const commands =
    mode === "localOnly" && recipes
      ? " Local only governs where models run, not the commands a recipe runs."
      : "";
  const browsing = browse
    ? mode === "localOnly"
      ? " Each new site is asked about once, and every form. Local only governs where models run, not the pages the browser opens."
      : " Each new site is asked about once, and every form."
    : "";
  // A skill is guidance the model is given, never a capability, so it is
  // said apart from what the task may ask to do (ADR 0053).
  const chosen =
    skills === 0
      ? ""
      : skills === 1
        ? " It is given one skill to follow."
        : ` It is given ${skills} skills to follow.`;
  return `${MODE_TEXT[mode]} ${access}${commands}${browsing}${chosen}`;
}

/** A new conversation's title, from the goal that starts it. */
export function titleFromGoal(goal: string): string {
  const line =
    goal
      .split("\n")
      .map((part) => part.trim())
      .find((part) => part !== "") ?? "";
  return line.length <= 60 ? line : `${line.slice(0, 59).trimEnd()}…`;
}
