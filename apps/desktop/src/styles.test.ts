/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

// Read from disk: the test runner hands CSS imports over empty, which made
// an earlier version of this test pass on nothing.
// Tests run from the desktop package's own folder.
const css = readFileSync(join(process.cwd(), "src", "styles.css"), "utf8");

/**
 * What jsdom cannot show: whether the stylesheet takes away the focus
 * outline anywhere without putting something in its place. A control whose
 * focus is invisible is one a keyboard user cannot find.
 */

/** Every rule, as its selector and its declarations, comments removed. */
function rules(): { selector: string; body: string }[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  return [...text.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((match) => ({
    selector: (match[1] ?? "").trim(),
    body: match[2] ?? "",
  }));
}

describe("the stylesheet", () => {
  it("never removes a focus outline without showing focus another way", () => {
    const all = rules();
    // Read whole, or the rest of this test would pass on nothing.
    expect(all.length).toBeGreaterThan(50);
    const removed = all.filter((rule) => /outline:\s*(0|none)\b/.test(rule.body));
    const missing = removed
      .map((rule) => /\.[\w-]+/.exec(rule.selector)?.[0] ?? rule.selector)
      .filter(
        (component) =>
          !all.some(
            (rule) =>
              rule.selector.includes(component) &&
              /:focus(-visible|-within)?/.test(rule.selector) &&
              /(outline|box-shadow|border)/.test(rule.body) &&
              !/outline:\s*(0|none)\b/.test(rule.body),
          ),
      );
    expect(missing).toEqual([]);
  });
});
