/**
 * Light, dark, or as the computer is: a preference of this window, kept in
 * its own storage like the Chat/Agent choice. Not a record, and not sent
 * anywhere.
 */

export type Theme = "system" | "light" | "dark";

const KEY = "tramontana.theme";

export function rememberedTheme(): Theme {
  try {
    const stored = localStorage.getItem(KEY);
    return stored === "light" || stored === "dark" ? stored : "system";
  } catch {
    return "system";
  }
}

/** Show `theme` now. System leaves the choice to the computer. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  if (theme === "system") delete root.dataset["theme"];
  else root.dataset["theme"] = theme;
}

/** Show `theme`, and remember it for the next time the window opens. */
export function chooseTheme(theme: Theme): void {
  // The whole window cross-fades where the engine can, so no part of it is
  // seen half way between light and dark.
  const reduced =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (typeof document.startViewTransition === "function" && !reduced) {
    document.startViewTransition(() => {
      applyTheme(theme);
    });
  } else {
    applyTheme(theme);
  }
  try {
    if (theme === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, theme);
  } catch {
    // A window that cannot remember still changes now.
  }
}
