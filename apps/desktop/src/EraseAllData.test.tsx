import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const eraseAllData = vi.hoisted(() => vi.fn());
const restartApplication = vi.hoisted(() => vi.fn());
const restartSupported = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  eraseAllData,
  restartApplication,
  restartSupported,
}));

const { EraseAllData } = await import("./EraseAllData.js");

beforeEach(() => {
  eraseAllData.mockResolvedValue({ schemaVersion: 1, credentialsRemoved: 1, credentialsAbsent: 0 });
  restartApplication.mockResolvedValue(undefined);
  restartSupported.mockResolvedValue(true);
});

describe("EraseAllData", () => {
  it("deletes only once the person types the word, then restarts", async () => {
    const user = userEvent.setup();
    render(<EraseAllData />);

    await user.click(screen.getByRole("button", { name: "Delete all my data…" }));
    const dialog = screen.getByRole("alertdialog");
    // It says what goes, and what stays.
    expect(dialog).toHaveTextContent("system keychain");
    expect(screen.getByText(/project folders and the files in them are not touched/)).toBeVisible();

    const confirm = screen.getByRole("button", { name: "Delete everything" });
    expect(confirm).toBeDisabled();
    await user.type(screen.getByLabelText(/to confirm/), "delet");
    expect(confirm).toBeDisabled();
    await user.type(screen.getByLabelText(/to confirm/), "e");
    expect(confirm).toBeEnabled();

    await user.click(confirm);
    expect(eraseAllData).toHaveBeenCalledOnce();
    expect(await screen.findByRole("status")).toHaveTextContent("Tramontana is restarting");
    expect(restartApplication).toHaveBeenCalledOnce();
  });

  it("shows a refusal, keeps everything, and does not restart", async () => {
    eraseAllData.mockRejectedValue({
      code: "conflict",
      message: "Stop the running tasks before deleting all data",
      retryable: true,
    });
    const user = userEvent.setup();
    render(<EraseAllData />);

    await user.click(screen.getByRole("button", { name: "Delete all my data…" }));
    await user.type(screen.getByLabelText(/to confirm/), "delete");
    await user.click(screen.getByRole("button", { name: "Delete everything" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Stop the running tasks before deleting all data",
    );
    expect(restartApplication).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("asks the person to start it again where it cannot restart itself", async () => {
    restartSupported.mockResolvedValue(false);
    const user = userEvent.setup();
    render(<EraseAllData />);

    await user.click(screen.getByRole("button", { name: "Delete all my data…" }));
    await user.type(screen.getByLabelText(/to confirm/), "delete");
    await user.click(screen.getByRole("button", { name: "Delete everything" }));

    expect(await screen.findByRole("status")).toHaveTextContent(
      "Quit Tramontana and start it again",
    );
    expect(restartApplication).not.toHaveBeenCalled();
  });
});
