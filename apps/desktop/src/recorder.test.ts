import { describe, expect, it } from "vitest";

import { SAMPLE_RATE, encodeWav, microphoneProblem } from "./recorder.js";

describe("encodeWav", () => {
  it("writes 16 kHz mono 16-bit PCM, as whisper.cpp reads it", () => {
    const wav = encodeWav(new Float32Array([0, 1, -1, 2]));
    const view = new DataView(wav.buffer);
    const text = (at: number): string => String.fromCharCode(...wav.slice(at, at + 4));

    expect(wav.length).toBe(44 + 8);
    expect(text(0)).toBe("RIFF");
    expect(text(8)).toBe("WAVE");
    expect(view.getUint16(20, true)).toBe(1);
    expect(view.getUint16(22, true)).toBe(1);
    expect(view.getUint32(24, true)).toBe(SAMPLE_RATE);
    expect(view.getUint16(34, true)).toBe(16);
    expect(view.getUint32(40, true)).toBe(8);
    expect([0, 1, 2, 3].map((index) => view.getInt16(44 + index * 2, true))).toEqual([
      0, 32767, -32768, 32767,
    ]);
  });
});

describe("microphoneProblem", () => {
  it("says plainly when the microphone was refused or missing", () => {
    expect(microphoneProblem(new DOMException("", "NotAllowedError"))).toContain(
      "not allowed to use the microphone",
    );
    expect(microphoneProblem(new DOMException("", "NotFoundError"))).toBe(
      "No microphone was found.",
    );
    expect(microphoneProblem(new Error("other"))).toContain("could not be used");
  });
});
