import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PersonalProfile } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const personalProfile = vi.hoisted(() => vi.fn());
const updatePersonal = vi.hoisted(() => vi.fn());
const remember = vi.hoisted(() => vi.fn());
const forget = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, personalProfile, updatePersonal, remember, forget };
});

const { SettingsView } = await import("./SettingsView.js");

const stored: PersonalProfile = {
  schemaVersion: 1,
  name: "Gasper",
  answerStyle: "balanced",
  memoryEnabled: true,
  notes: [{ noteId: "n1", text: "Our site is in Koper.", createdAt: "2026-09-28T00:00:00Z" }],
};

beforeEach(() => {
  for (const mock of [personalProfile, updatePersonal, remember, forget]) mock.mockReset();
  personalProfile.mockResolvedValue(stored);
});

describe("SettingsView", () => {
  it("shows what is stored, and saves only what changed, as the person wrote it", async () => {
    const user = userEvent.setup();
    updatePersonal.mockImplementation((details: { name: string; answerStyle: string }) =>
      Promise.resolve({ ...stored, name: details.name, answerStyle: details.answerStyle }),
    );
    render(<SettingsView />);

    const name = await screen.findByLabelText("What should Tramontana call you?");
    await waitFor(() => {
      expect(name).toHaveValue("Gasper");
    });
    const save = screen.getByRole("button", { name: "Save" });
    expect(save).toBeDisabled();

    await user.click(screen.getByRole("radio", { name: /Thorough/ }));
    await user.click(screen.getByRole("switch", { name: /Remember things between chats/ }));
    expect(save).toBeEnabled();
    await user.click(save);

    expect(updatePersonal).toHaveBeenCalledWith({
      name: "Gasper",
      about: "",
      answerStyle: "thorough",
      memoryEnabled: false,
    });
    expect(await screen.findByRole("status")).toHaveTextContent("Saved");
  });

  it("adds and forgets notes, and says nothing is remembered on its own", async () => {
    const user = userEvent.setup();
    remember.mockResolvedValue({
      ...stored,
      notes: [
        ...stored.notes,
        { noteId: "n2", text: "Prefers metric.", createdAt: "2026-09-28T00:00:01Z" },
      ],
    });
    forget.mockResolvedValue({ ...stored, notes: [] });
    render(<SettingsView />);

    expect(await screen.findByText("Our site is in Koper.")).toBeInTheDocument();
    expect(screen.getByText(/does not remember anything on its own/)).toBeInTheDocument();

    await user.type(screen.getByLabelText("New note"), "Prefers metric.");
    await user.click(screen.getByRole("button", { name: "Remember" }));
    expect(remember).toHaveBeenCalledWith("Prefers metric.");
    expect(await screen.findByText("Prefers metric.")).toBeInTheDocument();
    expect(screen.getByLabelText("New note")).toHaveValue("");

    await user.click(screen.getByRole("button", { name: "Forget “Our site is in Koper.”" }));
    expect(forget).toHaveBeenCalledWith("n1");
    expect(await screen.findByText("No notes yet.")).toBeInTheDocument();
  });

  it("says why a save was refused", async () => {
    const user = userEvent.setup();
    updatePersonal.mockRejectedValue({
      schemaVersion: 1,
      code: "invalidRequest",
      message: "A name must be at most 100 bytes of plain text",
      retryable: false,
      correlationId: "c",
    });
    render(<SettingsView />);
    const about = await screen.findByLabelText("What do you do at work?");
    await user.type(about, "Cranes.");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("at most 100 bytes");
  });
});
