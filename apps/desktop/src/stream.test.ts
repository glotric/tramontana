import { describe, expect, it } from "vitest";
import type { StreamFrame } from "@tramontana/shared";

import { foldFrame, type LiveStream } from "./stream.js";

function frame(streamId: string, offset: number, text: string): StreamFrame {
  return { streamId, offset, text };
}

/** Fold a whole sequence of frames, as a window does as they arrive. */
function fold(...frames: readonly StreamFrame[]): LiveStream | undefined {
  let stream: LiveStream | undefined;
  for (const one of frames) stream = foldFrame(stream, one);
  return stream;
}

describe("the answer being produced right now", () => {
  it("reads as one answer when its frames arrive from the start", () => {
    const stream = fold(frame("s1", 0, "Hello, "), frame("s1", 7, "world."));

    expect(stream?.text).toBe("Hello, world.");
    expect(stream?.partial).toBe(false);
  });

  it("starts again when the next model turn starts, rather than adding to the last", () => {
    const stream = fold(
      frame("s1", 0, "First answer."),
      frame("s2", 0, "Second "),
      frame("s2", 7, "answer."),
    );

    expect(stream?.text).toBe("Second answer.");
    expect(stream?.streamId).toBe("s2");
    // A second turn is not a continuation of the first: what the model said
    // before is in the record, not on the end of what it is saying now.
    expect(stream?.partial).toBe(false);
  });

  it("says the beginning is missing when this window joined part-way", () => {
    // What a reload mid-answer delivers: the stream is under way, and the
    // frames before it are gone. Showing the tail as if it were the answer
    // would misreport what the model said.
    const stream = fold(frame("s1", 7, "world."));

    expect(stream?.text).toBe("world.");
    expect(stream?.partial).toBe(true);
  });

  it("keeps saying so once it has joined part-way", () => {
    const stream = fold(frame("s1", 7, "wor"), frame("s1", 10, "ld."));

    expect(stream?.text).toBe("world.");
    expect(stream?.partial).toBe(true);
  });

  it("does not show a frame twice", () => {
    const repeated = frame("s1", 7, "world.");
    const stream = fold(frame("s1", 0, "Hello, "), repeated, repeated);

    expect(stream?.text).toBe("Hello, world.");
    expect(stream?.partial).toBe(false);
  });

  it("is not rewound by a frame it passed long ago", () => {
    // An earlier frame delivered again, or out of order. Holding less than it
    // does would make the next frame look like a gap in an answer that has
    // none, so what is held must not move backwards.
    const stream = fold(
      frame("s1", 0, "Hello, "),
      frame("s1", 7, "world"),
      frame("s1", 0, "Hello, "),
      frame("s1", 12, "."),
    );

    expect(stream?.text).toBe("Hello, world.");
    expect(stream?.partial).toBe(false);
  });

  it("adds only the part of an overlapping frame it does not already hold", () => {
    const stream = fold(frame("s1", 0, "Hello, wor"), frame("s1", 7, "world."));

    expect(stream?.text).toBe("Hello, world.");
    expect(stream?.partial).toBe(false);
  });

  it("says the answer is no longer whole when a frame never arrived", () => {
    const stream = fold(frame("s1", 0, "Hello, "), frame("s1", 20, "world."));

    expect(stream?.text).toBe("Hello, world.");
    expect(stream?.partial).toBe(true);
  });

  it("measures a frame's position in bytes, as the core counts it", () => {
    // Offsets come from the model turn's accumulated bytes. A character that
    // is one unit of text and three bytes long would otherwise make every
    // later frame look like a gap.
    const stream = fold(frame("s1", 0, "héllo"), frame("s1", 6, " world"));

    expect(stream?.text).toBe("héllo world");
    expect(stream?.partial).toBe(false);
  });

  it("drops the bytes it holds from an overlapping frame without splitting a character", () => {
    const stream = fold(frame("s1", 0, "héllo"), frame("s1", 1, "éllo there"));

    expect(stream?.text).toBe("héllo there");
    expect(stream?.partial).toBe(false);
  });
});
