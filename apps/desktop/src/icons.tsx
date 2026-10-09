/**
 * The plain line icons of the sidebar, header and composer. Drawn in the
 * current text colour, and always beside words or an accessible name: none
 * of them says anything on its own.
 */

const PATHS = {
  sidebar: (
    <>
      <rect x="3" y="4" width="18" height="16" rx="3" />
      <path d="M9 4v16" />
    </>
  ),
  close: <path d="M6 6l12 12M18 6 6 18" />,
  share: (
    <>
      <path d="M12 15V3M7 8l5-5 5 5" />
      <path d="M5 13v6a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-6" />
    </>
  ),
  lock: (
    <>
      <rect x="5" y="11" width="14" height="10" rx="2" />
      <path d="M8 11V7a4 4 0 0 1 8 0v4" />
    </>
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="6.5" />
      <path d="m16 16 4.5 4.5" />
    </>
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  clock: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  chevronRight: <path d="m9 6 6 6-6 6" />,
  chevronDown: <path d="m6 9 6 6 6-6" />,
  folder: (
    <path d="M3.5 7.5a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2Z" />
  ),
  grid: (
    <>
      <rect x="4" y="4" width="6.5" height="6.5" rx="1.5" />
      <rect x="13.5" y="4" width="6.5" height="6.5" rx="1.5" />
      <rect x="4" y="13.5" width="6.5" height="6.5" rx="1.5" />
      <rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.5" />
    </>
  ),
  cpu: (
    <>
      <rect x="6.5" y="6.5" width="11" height="11" rx="2" />
      <path d="M10 3v3.5M14 3v3.5M10 17.5V21M14 17.5V21M3 10h3.5M3 14h3.5M17.5 10H21M17.5 14H21" />
    </>
  ),
  apps: (
    <>
      <path d="M12 3.5 20 8v8l-8 4.5L4 16V8Z" />
      <path d="m4 8 8 4.5L20 8M12 12.5v8" />
    </>
  ),
  gear: (
    <>
      <path d="M10.27 4.80 L10.80 2.48 L13.20 2.48 L13.73 4.80 L15.87 5.69 L17.88 4.41 L19.59 6.12 L18.31 8.13 L19.20 10.27 L21.52 10.80 L21.52 13.20 L19.20 13.73 L18.31 15.87 L19.59 17.88 L17.88 19.59 L15.87 18.31 L13.73 19.20 L13.20 21.52 L10.80 21.52 L10.27 19.20 L8.13 18.31 L6.12 19.59 L4.41 17.88 L5.69 15.87 L4.80 13.73 L2.48 13.20 L2.48 10.80 L4.80 10.27 L5.69 8.13 L4.41 6.12 L6.12 4.41 L8.13 5.69Z" />
      <circle cx="12" cy="12" r="3.2" />
    </>
  ),
  shield: <path d="M12 3.5 19 6v5.5c0 4.2-2.9 7.6-7 9-4.1-1.4-7-4.8-7-9V6Z" />,
  globe: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17M12 3.5c2.4 2.4 3.5 5.2 3.5 8.5s-1.1 6.1-3.5 8.5c-2.4-2.4-3.5-5.2-3.5-8.5S9.6 5.9 12 3.5Z" />
    </>
  ),
  sparkle: <path d="M12 3.5 13.8 10 20.5 12 13.8 14 12 20.5 10.2 14 3.5 12 10.2 10Z" />,
  panel: (
    <>
      <rect x="3" y="4" width="18" height="16" rx="3" />
      <path d="M15 4v16" />
    </>
  ),
  send: <path d="M12 19V5M5 12l7-7 7 7" />,
  terminal: (
    <>
      <rect x="3" y="4.5" width="18" height="15" rx="3" />
      <path d="m7.5 9.5 3 2.5-3 2.5M12.5 15h4" />
    </>
  ),
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({
  name,
  size = 18,
  strokeWidth = 1.8,
}: {
  readonly name: IconName;
  readonly size?: number;
  readonly strokeWidth?: number;
}): React.JSX.Element {
  return (
    <svg
      className="icon"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {PATHS[name]}
    </svg>
  );
}
