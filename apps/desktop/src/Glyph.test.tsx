import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Glyph } from "./Glyph.js";

describe("Glyph", () => {
  it("is decorative, so it adds nothing to a control's name", () => {
    const { container } = render(
      <button type="button">
        <Glyph icon="folder" hue="yellow" />
        Open project
      </button>,
    );
    const svg = container.querySelector("svg");
    expect(svg).toHaveAttribute("aria-hidden", "true");
    expect(container.querySelector("button")).toHaveAccessibleName("Open project");
  });

  it("gives each icon its own gradient ids, so two on one page never share one", () => {
    const { container } = render(
      <>
        <Glyph icon="chat" />
        <Glyph icon="chat" />
      </>,
    );
    const ids = [...container.querySelectorAll("linearGradient")].map((node) => node.id);
    expect(ids).toHaveLength(4);
    expect(new Set(ids).size).toBe(4);
    // Colons from useId would break the url(#…) references.
    for (const id of ids) expect(id).toMatch(/^[a-zA-Z0-9]+$/);
  });
});
