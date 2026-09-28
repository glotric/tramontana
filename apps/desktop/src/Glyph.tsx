import { useId } from "react";

/**
 * The frosted-glass icon from the design system: a tinted shape behind a
 * translucent pane, with a white detail stroke. Purely decorative — every
 * place that shows one also says what it is in words.
 */

export type GlyphHue = "blue" | "green" | "orange" | "red" | "yellow" | "purple" | "teal";

export type GlyphIcon =
  | "folder"
  | "clock"
  | "check"
  | "chat"
  | "agent"
  | "doc"
  | "chart"
  | "shield"
  | "wind"
  | "search"
  | "pen"
  | "gear"
  | "lock"
  | "plus"
  | "book"
  | "wrench"
  | "board"
  | "inbox";

const HUES: Record<GlyphHue, readonly [string, string]> = {
  blue: ["#1d5cff", "#62c8ff"],
  green: ["#0fbf5a", "#8ef2c6"],
  orange: ["#ff4a17", "#ffaa4d"],
  red: ["#ff2657", "#ff9db4"],
  yellow: ["#ffa800", "#ffe066"],
  purple: ["#6a45ff", "#c9abff"],
  teal: ["#00a99a", "#7df2e2"],
};

function circle(cx: number, cy: number, r: number): string {
  return `M${cx - r} ${cy}a${r} ${r} 0 1 0 ${2 * r} 0a${r} ${r} 0 1 0 ${-2 * r} 0Z`;
}

function gear(cx: number, cy: number, n: number, ro: number, ri: number): string {
  const step = (Math.PI * 2) / n;
  let d = "";
  for (let i = 0; i < n; i += 1) {
    const a = i * step - Math.PI / 2;
    const points: readonly (readonly [number, number])[] = [
      [ri, a - step * 0.5],
      [ro, a - step * 0.22],
      [ro, a + step * 0.22],
      [ri, a + step * 0.5],
    ];
    points.forEach(([r, angle], j) => {
      d += `${i === 0 && j === 0 ? "M" : "L"}${(cx + r * Math.cos(angle)).toFixed(1)} ${(cy + r * Math.sin(angle)).toFixed(1)}`;
    });
  }
  return `${d}Z`;
}

const SHAPES: Record<GlyphIcon, { readonly front: string; readonly detail: string }> = {
  folder: {
    front: "M4 22a6 6 0 0 1 6-6h13l6 6h19a6 6 0 0 1 6 6v24a6 6 0 0 1-6 6H10a6 6 0 0 1-6-6Z",
    detail: "M14 38h26",
  },
  clock: { front: circle(29, 35, 24), detail: "M29 22v13l9 6" },
  check: { front: circle(29, 35, 24), detail: "M18 35l8 8 14-15" },
  chat: {
    front: "M14 12h32a8 8 0 0 1 8 8v20a8 8 0 0 1-8 8H27l-13 10V48a8 8 0 0 1-8-8V20a8 8 0 0 1 8-8Z",
    detail: "M19 30h.01M30 30h.01M41 30h.01",
  },
  agent: {
    front: "M4 20a7 7 0 0 1 7-7h38a7 7 0 0 1 7 7v30a7 7 0 0 1-7 7H11a7 7 0 0 1-7-7Z",
    detail: "M11 20h.01M17 20h.01M23 20h.01M14 32l7 6-7 6M27 45h13",
  },
  doc: {
    front: "M15 6h21l14 14v34a6 6 0 0 1-6 6H15a6 6 0 0 1-6-6V12a6 6 0 0 1 6-6Z",
    detail: "M36 7v13h13M18 35h20M18 44h13",
  },
  chart: {
    front:
      "M5 38a4 4 0 0 1 4-4h8a4 4 0 0 1 4 4v20H5ZM23 26a4 4 0 0 1 4-4h8a4 4 0 0 1 4 4v32H23ZM41 14a4 4 0 0 1 4-4h8a4 4 0 0 1 4 4v44H41Z",
    detail: "",
  },
  shield: {
    front: "M29 7l23 8v15c0 14-9 25-23 30C15 55 6 44 6 30V15Z",
    detail: "M19 33l7 7 13-14",
  },
  wind: {
    front: "M4 20a12 12 0 0 1 12-12h28a12 12 0 0 1 12 12v28a12 12 0 0 1-12 12H16A12 12 0 0 1 4 48Z",
    detail: "M13 27h20a5 5 0 1 0-5-5M13 35h26a5 5 0 1 1-5 5M13 43h12",
  },
  search: {
    front: `${circle(26, 30, 20)}M39 45l5-5 14 14a3.5 3.5 0 0 1-5 5Z`,
    detail: circle(26, 30, 10),
  },
  pen: { front: "M44 5l15 15-33 33-18 5 5-18Z", detail: "M37 12l15 15M14 44l6 6" },
  gear: { front: gear(29, 35, 8, 26, 19), detail: circle(29, 35, 8) },
  lock: {
    front:
      "M15 26v-6a14 14 0 0 1 28 0v6h-6v-6a8 8 0 0 0-16 0v6ZM9 32a6 6 0 0 1 6-6h28a6 6 0 0 1 6 6v20a6 6 0 0 1-6 6H15a6 6 0 0 1-6-6Z",
    detail: "M29 37v10",
  },
  plus: {
    front:
      "M23 8h12a2 2 0 0 1 2 2v15h15a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H37v15a2 2 0 0 1-2 2H23a2 2 0 0 1-2-2V41H6a2 2 0 0 1-2-2V27a2 2 0 0 1 2-2h15V10a2 2 0 0 1 2-2Z",
    detail: "",
  },
  book: {
    front: "M9 13a7 7 0 0 1 7-7h34v52H16a7 7 0 0 1-7-7Z",
    detail: "M18 7v50M27 21h14M27 30h10",
  },
  wrench: {
    front:
      "M45 6a14 14 0 0 0-14 18L8 47a6.5 6.5 0 0 0 9 9l23-23a14 14 0 0 0 18-14l-9 7-8-2-2-8 9-7a14 14 0 0 0-3-3Z",
    detail: "",
  },
  board: {
    front: "M6 12h52v30a5 5 0 0 1-5 5H11a5 5 0 0 1-5-5ZM29 47h6v13h-6Z",
    detail: "M14 36l9-9 7 6 13-12",
  },
  inbox: {
    front: "M4 34l10-22h36l10 22v17a6 6 0 0 1-6 6H10a6 6 0 0 1-6-6Z",
    detail: "M5 34h14l4 7h18l4-7h14",
  },
};

export function Glyph({
  icon,
  hue = "blue",
  size = 28,
}: {
  readonly icon: GlyphIcon;
  readonly hue?: GlyphHue;
  readonly size?: number;
}): React.JSX.Element {
  // useId gives colons, which are not safe inside url(#…) references.
  const uid = `g${useId().replace(/[^a-zA-Z0-9]/g, "")}`;
  const [c1, c2] = HUES[hue];
  const shape = SHAPES[icon];
  const n = parseInt(c1.slice(1), 16);
  const rgb = `${(n >> 16) & 255},${(n >> 8) & 255},${n & 255}`;
  const shadow = `drop-shadow(0 ${Math.max(1, Math.round(size * 0.06))}px ${Math.max(2, Math.round(size * 0.12))}px rgba(${rgb},0.35))`;
  return (
    <svg
      className="glyph"
      aria-hidden="true"
      focusable="false"
      width={size}
      height={size}
      viewBox="0 0 64 64"
      style={{ filter: shadow }}
    >
      <defs>
        <linearGradient id={`${uid}b`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor={c2} />
          <stop offset="1" stopColor={c1} />
        </linearGradient>
        <linearGradient id={`${uid}g`} x1="0.1" y1="0" x2="0.9" y2="1">
          <stop offset="0" stopColor="#ffffff" stopOpacity="0.78" />
          <stop offset="0.5" stopColor={c2} stopOpacity="0.42" />
          <stop offset="1" stopColor={c1} stopOpacity="0.34" />
        </linearGradient>
        <clipPath id={`${uid}c`}>
          <path d={shape.front} />
        </clipPath>
        <filter id={`${uid}f`} x="-20%" y="-20%" width="140%" height="140%">
          <feGaussianBlur stdDeviation="3.2" />
        </filter>
      </defs>
      <path d={shape.front} transform="translate(4 -4)" fill={`url(#${uid}b)`} />
      <g clipPath={`url(#${uid}c)`}>
        <path
          d={shape.front}
          transform="translate(4 -4)"
          fill={`url(#${uid}b)`}
          filter={`url(#${uid}f)`}
        />
      </g>
      <path d={shape.front} fill={`url(#${uid}g)`} />
      <path
        d={shape.front}
        fill="none"
        stroke="#ffffff"
        strokeOpacity="0.75"
        strokeWidth="1.2"
        strokeLinejoin="round"
      />
      {shape.detail !== "" && (
        <path
          d={shape.detail}
          fill="none"
          stroke="#ffffff"
          strokeWidth={size < 40 ? 5 : 4}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      )}
    </svg>
  );
}
