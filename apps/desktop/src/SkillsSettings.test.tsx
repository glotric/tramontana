import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PersonalSkill } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const listPersonalSkills = vi.hoisted(() => vi.fn());
const importSkill = vi.hoisted(() => vi.fn());
const removeSkill = vi.hoisted(() => vi.fn());
const skillsFolder = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, listPersonalSkills, importSkill, removeSkill, skillsFolder };
});

const { SkillsSettings } = await import("./SkillsSettings.js");

const skill = (id: string, name: string): PersonalSkill => ({
  schemaVersion: 1,
  id,
  name,
  description: `${name}, described.`,
  contentHash: id,
  byteCount: 2048,
});

beforeEach(() => {
  skillsFolder.mockResolvedValue("/Users/maja/Library/Application Support/app/skills");
});

describe("SkillsSettings", () => {
  it("says where skills are kept and that nothing in them runs", async () => {
    listPersonalSkills.mockResolvedValue([]);
    render(<SkillsSettings />);
    expect(await screen.findByText("You have no skills of your own yet.")).toBeTruthy();
    expect(await screen.findByText(/app\/skills$/)).toBeTruthy();
    expect(screen.getByText(/nothing in a skill is run/)).toBeTruthy();
  });

  it("imports a skill folder or file, and lists it", async () => {
    const user = userEvent.setup();
    listPersonalSkills
      .mockResolvedValueOnce([])
      .mockResolvedValue([skill("pdf-forms", "PDF Forms")]);
    importSkill
      .mockResolvedValueOnce(skill("pdf-forms", "PDF Forms"))
      .mockResolvedValueOnce(undefined);
    render(<SkillsSettings />);

    await user.click(await screen.findByRole("button", { name: "Import a skill folder…" }));
    expect(importSkill).toHaveBeenLastCalledWith(true);
    expect(await screen.findByRole("status")).toHaveTextContent("Added /pdf-forms.");
    expect(await screen.findByText("/pdf-forms")).toBeTruthy();

    // A cancelled picker changes nothing.
    await user.click(screen.getByRole("button", { name: "Import a .zip or .md file…" }));
    expect(importSkill).toHaveBeenLastCalledWith(false);
  });

  it("removes a skill only after asking", async () => {
    const user = userEvent.setup();
    listPersonalSkills.mockResolvedValueOnce([skill("tone", "Tone")]).mockResolvedValue([]);
    removeSkill.mockResolvedValue(undefined);
    render(<SkillsSettings />);

    await user.click(await screen.findByRole("button", { name: "Remove the skill Tone" }));
    const ask = screen.getByRole("group", { name: "Remove /tone?" });
    await user.click(within(ask).getByRole("button", { name: "Keep" }));
    expect(removeSkill).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Remove the skill Tone" }));
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => {
      expect(removeSkill).toHaveBeenCalledWith("tone");
    });
    expect(await screen.findByText("You have no skills of your own yet.")).toBeTruthy();
  });
});
