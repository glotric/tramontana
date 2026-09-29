/**
 * Recording speech for dictation (ADR 0065), as the 16 kHz mono 16-bit WAV
 * that whisper.cpp reads. The webview records in its own format
 * (`MediaRecorder`), which is decoded and converted here. Nothing is sent
 * anywhere from here.
 */

/** whisper.cpp's sample rate. */
export const SAMPLE_RATE = 16_000;

/** The longest recording, in seconds. */
export const MAX_SECONDS = 300;

/** 16 kHz mono samples as a 16-bit PCM WAV file. */
export function encodeWav(samples: Float32Array): Uint8Array {
  const bytes = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(bytes);
  const text = (offset: number, value: string): void => {
    for (let index = 0; index < value.length; index += 1) {
      view.setUint8(offset + index, value.charCodeAt(index));
    }
  };
  text(0, "RIFF");
  view.setUint32(4, 36 + samples.length * 2, true);
  text(8, "WAVE");
  text(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, SAMPLE_RATE, true);
  view.setUint32(28, SAMPLE_RATE * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, samples.length * 2, true);
  samples.forEach((sample, index) => {
    const clamped = Math.max(-1, Math.min(1, sample));
    view.setInt16(44 + index * 2, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
  });
  return new Uint8Array(bytes);
}

/** A recording under way. */
export interface Recording {
  /** Stop, and return the recording as WAV. */
  readonly stop: () => Promise<Uint8Array>;
  /** Stop and throw the recording away. */
  readonly cancel: () => void;
}

/** Why the microphone could not be used, in words. */
export function microphoneProblem(error: unknown): string {
  const name = error instanceof DOMException ? error.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return "Tramontana was not allowed to use the microphone. Allow it in your system's privacy settings.";
  }
  if (name === "NotFoundError") return "No microphone was found.";
  return "The microphone could not be used on this computer.";
}

/** Decode a recording in whatever format the webview made, as 16 kHz mono. */
async function toSpeechSamples(recorded: Blob): Promise<Float32Array> {
  const decoder = new AudioContext();
  try {
    const decoded = await decoder.decodeAudioData(await recorded.arrayBuffer());
    const length = Math.max(1, Math.ceil(decoded.duration * SAMPLE_RATE));
    // Rendering to one channel at 16 kHz downmixes and resamples.
    const render = new OfflineAudioContext(1, length, SAMPLE_RATE);
    const source = render.createBufferSource();
    source.buffer = decoded;
    source.connect(render.destination);
    source.start();
    return (await render.startRendering()).getChannelData(0);
  } finally {
    void decoder.close();
  }
}

/**
 * Start recording from the microphone.
 *
 * @throws when the webview has no microphone API, or access is refused; see
 * {@link microphoneProblem}.
 */
export async function startRecording(): Promise<Recording> {
  if (!("mediaDevices" in navigator) || typeof MediaRecorder === "undefined") {
    throw new DOMException("No microphone API", "NotSupportedError");
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
  });
  const release = (): void => {
    for (const track of stream.getTracks()) track.stop();
  };
  let recorder: MediaRecorder;
  try {
    recorder = new MediaRecorder(stream);
  } catch (failure: unknown) {
    release();
    throw failure;
  }
  const chunks: Blob[] = [];
  recorder.ondataavailable = (event) => {
    if (event.data.size > 0) chunks.push(event.data);
  };
  recorder.start();

  return {
    stop: () =>
      new Promise<Blob>((resolve) => {
        recorder.onstop = () => {
          release();
          resolve(new Blob(chunks, { type: recorder.mimeType }));
        };
        recorder.stop();
      })
        .then(toSpeechSamples)
        .then(encodeWav),
    cancel: () => {
      recorder.onstop = null;
      if (recorder.state !== "inactive") recorder.stop();
      release();
    },
  };
}
