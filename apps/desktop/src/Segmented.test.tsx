import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";

import { Segmented } from "./Segmented.js";

function Harness({ kind }: { readonly kind: "radios" | "tabs" }): React.JSX.Element {
  const [value, setValue] = useState("a");
  return (
    <Segmented
      kind={kind}
      label="Letters"
      idPrefix="t"
      panelId="p"
      value={value}
      options={[
        { value: "a", label: "A" },
        { value: "b", label: "B" },
        { value: "c", label: "C" },
      ]}
      onChange={setValue}
    />
  );
}

describe("Segmented", () => {
  it("keeps only the chosen radio in the Tab order and moves with the arrow keys", async () => {
    const user = userEvent.setup();
    render(<Harness kind="radios" />);
    expect(screen.getByRole("radiogroup", { name: "Letters" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "A" })).toHaveAttribute("tabindex", "0");
    expect(screen.getByRole("radio", { name: "B" })).toHaveAttribute("tabindex", "-1");

    await user.tab();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("radio", { name: "B" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("radio", { name: "B" })).toHaveFocus();

    await user.keyboard("{End}{ArrowRight}");
    expect(screen.getByRole("radio", { name: "A" })).toHaveAttribute("aria-checked", "true");
  });

  it("names the tab and the panel it controls", () => {
    render(<Harness kind="tabs" />);
    const tab = screen.getByRole("tab", { name: "A" });
    expect(tab).toHaveAttribute("aria-selected", "true");
    expect(tab).toHaveAttribute("aria-controls", "p");
    expect(tab).toHaveAttribute("id", "t-tab-a");
  });
});
