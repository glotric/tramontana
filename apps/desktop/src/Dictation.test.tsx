import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

const dictationStatus = vi.hoisted(() => vi.fn());
const chooseDictation = vi.hoisted(() => vi.fn());
const clearDictation = vi.hoisted(() => vi.fn());
const transcribe = vi.hoisted(() => vi.fn());
const startRecording = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  dictationStatus,
  chooseDictation,
  clearDictation,
  transcribe,
}));
vi.mock(import("./recorder.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  startRecording,
}));

const { DictateButton, DictationSettings, appendDictated, useDictation } =
  await import("./Dictation.js");

function Composer(): React.JSX.Element {
  const [draft, setDraft] = useState("Plan");
  const dictation = useDictation((text) => {
    setDraft((current) => appendDictated(current, text));
  });
  return (
    <>
      {dictation.error !== undefined && <p role="alert">{dictation.error}</p>}
      <output aria-label="Draft">{draft}</output>
      <DictateButton
        state={dictation.state}
        seconds={dictation.seconds}
        onToggle={dictation.toggle}
      />
    </>
  );
}

const configured = {
  configured: true,
  binaryPath: "/opt/whisper/whisper-cli",
  modelPath: "/models/ggml-base.bin",
  language: "en",
};

describe("appendDictated", () => {
  it("puts the words after what is written, with one space", () => {
    expect(appendDictated("", "Hello.")).toBe("Hello.");
    expect(appendDictated("Plan", "my week.")).toBe("Plan my week.");
    expect(appendDictated("Plan ", "my week.")).toBe("Plan my week.");
  });
});

describe("dictation in a composer", () => {
  it("records, transcribes on the device, and adds the words to the draft", async () => {
    const user = userEvent.setup();
    const wav = new Uint8Array([82, 73, 70, 70]);
    const cancel = vi.fn();
    dictationStatus.mockResolvedValue(configured);
    startRecording.mockResolvedValue({ stop: () => Promise.resolve(wav), cancel });
    transcribe.mockResolvedValue("my week.");
    render(<Composer />);

    await user.click(screen.getByRole("button", { name: "Dictate" }));
    const stop = await screen.findByRole("button", { name: "Stop dictating" });
    expect(stop).toHaveAttribute("aria-pressed", "true");
    expect(stop).toHaveTextContent("0:00");

    await user.click(stop);
    await waitFor(() => {
      expect(screen.getByLabelText("Draft")).toHaveTextContent("Plan my week.");
    });
    expect(transcribe).toHaveBeenCalledWith(wav);
    expect(screen.getByRole("button", { name: "Dictate" })).toBeEnabled();
  });

  it("explains what to set up, and never opens the microphone, when it is not set up", async () => {
    const user = userEvent.setup();
    dictationStatus.mockResolvedValue({ configured: false });
    render(<Composer />);

    await user.click(screen.getByRole("button", { name: "Dictate" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /In Settings, choose a whisper\.cpp/,
    );
    expect(startRecording).not.toHaveBeenCalled();
  });

  it("says so when the microphone is refused", async () => {
    const user = userEvent.setup();
    dictationStatus.mockResolvedValue(configured);
    startRecording.mockRejectedValue(new DOMException("denied", "NotAllowedError"));
    render(<Composer />);

    await user.click(screen.getByRole("button", { name: "Dictate" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("not allowed to use the microphone");
    expect(transcribe).not.toHaveBeenCalled();
  });

  it("shows a failed transcription and leaves the draft as it was", async () => {
    const user = userEvent.setup();
    dictationStatus.mockResolvedValue(configured);
    startRecording.mockResolvedValue({
      stop: () => Promise.resolve(new Uint8Array()),
      cancel: vi.fn(),
    });
    transcribe.mockRejectedValue({
      schemaVersion: 1,
      code: "WORKER_UNAVAILABLE",
      message: "No speech was heard in the recording.",
      retryable: false,
    });
    render(<Composer />);

    await user.click(screen.getByRole("button", { name: "Dictate" }));
    await user.click(await screen.findByRole("button", { name: "Stop dictating" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("No speech was heard");
    expect(screen.getByLabelText("Draft")).toHaveTextContent(/^Plan$/);
  });

  it("stops the microphone when the composer goes away", async () => {
    const user = userEvent.setup();
    const cancel = vi.fn();
    dictationStatus.mockResolvedValue(configured);
    startRecording.mockResolvedValue({ stop: vi.fn(), cancel });
    const { unmount } = render(<Composer />);

    await user.click(screen.getByRole("button", { name: "Dictate" }));
    await screen.findByRole("button", { name: "Stop dictating" });
    unmount();
    expect(cancel).toHaveBeenCalled();
  });
});

describe("DictationSettings", () => {
  it("chooses the program and model with the language given, and removes the setup", async () => {
    const user = userEvent.setup();
    dictationStatus.mockResolvedValue({ configured: false });
    chooseDictation.mockResolvedValue(configured);
    clearDictation.mockResolvedValue({ configured: false });
    render(<DictationSettings />);

    expect(await screen.findByText("Not set up.")).toBeInTheDocument();
    expect(screen.getByText(/Nothing is\s+downloaded/)).toBeInTheDocument();
    await user.type(screen.getByLabelText(/Language code/), "en");
    await user.click(screen.getByRole("button", { name: /Choose whisper-cli/ }));
    expect(chooseDictation).toHaveBeenCalledWith("en");
    expect(await screen.findByText("/models/ggml-base.bin")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Remove" }));
    expect(await screen.findByText("Not set up.")).toBeInTheDocument();
  });

  it("keeps the setup when the picker is cancelled", async () => {
    const user = userEvent.setup();
    dictationStatus.mockResolvedValue(configured);
    chooseDictation.mockResolvedValue(undefined);
    render(<DictationSettings />);

    await user.click(await screen.findByRole("button", { name: "Choose again…" }));
    expect(chooseDictation).toHaveBeenCalledWith("en");
    expect(screen.getByText("/opt/whisper/whisper-cli")).toBeInTheDocument();
  });
});
