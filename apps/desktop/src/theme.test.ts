import { afterEach, describe, expect, it } from "vitest";

import { applyTheme, chooseTheme, rememberedTheme } from "./theme.js";

afterEach(() => {
  applyTheme("system");
  localStorage.clear();
});

describe("theme", () => {
  it("follows the computer until the person chooses, and remembers the choice", () => {
    expect(rememberedTheme()).toBe("system");
    expect(document.documentElement.dataset["theme"]).toBeUndefined();

    chooseTheme("dark");
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(rememberedTheme()).toBe("dark");

    chooseTheme("system");
    expect(document.documentElement.dataset["theme"]).toBeUndefined();
    expect(rememberedTheme()).toBe("system");
  });
});
