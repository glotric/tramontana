import type { StreamFrame } from "@tramontana/shared";

/**
 * The answer a model is producing right now, as this window has seen it.
 *
 * Frames are transport, not record (`CORE_CONTRACTS.md` §5): they are not
 * persisted, and a window that reloads mid-answer joins the stream wherever it
 * has got to. So a frame cannot simply be appended to whatever is on screen.
 * Each carries the stream it belongs to and its position from that stream's
 * start, and both are needed to show a person something true: text from a new
 * stream is a new answer, text that repeats what is already held is not shown
 * twice, and text that begins past what is held is the middle of an answer
 * whose beginning this window never saw.
 *
 * The whole answer arrives durably as `messageCommitted` when the turn ends,
 * and that is what the reader keeps. This is only what is visible while it is
 * still being produced.
 */
export interface LiveStream {
  /** Which model turn this text belongs to; a new one is a new answer. */
  readonly streamId: string;
  /** Where the held text starts, in bytes from the stream's start. */
  readonly from: number;
  /** Where the held text ends, in bytes from the stream's start. */
  readonly end: number;
  readonly text: string;
  /**
   * Whether the beginning is missing: this window joined the stream after it
   * started, or a frame never arrived. What is on screen is then part of an
   * answer rather than the answer, and saying so is the difference between
   * honest and misleading.
   */
  readonly partial: boolean;
}

/** Offsets are byte positions, so text is measured the same way. */
function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** Drop the first `bytes` bytes of `text`, which this window already holds. */
function afterBytes(text: string, bytes: number): string {
  const encoded = new TextEncoder().encode(text);
  return new TextDecoder().decode(encoded.subarray(bytes));
}

/** Where a frame begins a stream this window has not been following. */
function begin(frame: StreamFrame): LiveStream {
  return {
    streamId: frame.streamId,
    from: frame.offset,
    end: frame.offset + byteLength(frame.text),
    text: frame.text,
    // Anything but the stream's own beginning means the beginning is missing.
    partial: frame.offset > 0,
  };
}

/**
 * Add one frame to what this window holds.
 *
 * Pure, and total: every frame produces a stream, because refusing one would
 * lose output rather than mislabel it.
 */
export function foldFrame(current: LiveStream | undefined, frame: StreamFrame): LiveStream {
  if (current?.streamId !== frame.streamId) return begin(frame);

  const added = byteLength(frame.text);
  // Already held, whole. A frame delivered twice must not be read twice.
  if (frame.offset + added <= current.end) return current;

  if (frame.offset > current.end) {
    // A gap: frames between what is held and this one never arrived, so the
    // text is no longer continuous and cannot be presented as whole.
    return {
      ...current,
      end: frame.offset + added,
      text: current.text + frame.text,
      partial: true,
    };
  }

  // Contiguous, or overlapping what is held; only the new part is added.
  const seen = current.end - frame.offset;
  return {
    ...current,
    end: frame.offset + added,
    text: current.text + (seen === 0 ? frame.text : afterBytes(frame.text, seen)),
  };
}
