import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { ChipMenu } from "./ChipMenu.js";

function chip() {
  return (
    <ChipMenu name="Where models run" value="Automatic" icon="cpu">
      {(close) => (
        <div>
          <button type="button" onClick={close}>
            Local only
          </button>
          <button type="button">Cloud preferred</button>
        </div>
      )}
    </ChipMenu>
  );
}

describe("ChipMenu", () => {
  it("moves focus into the panel when it opens, and back to the chip on Escape", async () => {
    const user = userEvent.setup();
    render(chip());
    const button = screen.getByRole("button", { name: "Where models run: Automatic" });

    await user.click(button);
    expect(screen.getByRole("button", { name: "Local only" })).toHaveFocus();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(button).toHaveFocus();
  });

  it("returns focus to the chip once a choice closes the panel", async () => {
    const user = userEvent.setup();
    render(chip());
    const button = screen.getByRole("button", { name: "Where models run: Automatic" });

    await user.click(button);
    await user.click(screen.getByRole("button", { name: "Local only" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(button).toHaveFocus();
  });
});
