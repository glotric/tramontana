import { useEffect, useId, useRef, useState } from "react";

import { Icon } from "./icons.js";
import type { IconName } from "./icons.js";

/** What can take focus inside a panel, in document order. */
const FOCUSABLE =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * A composer chip: a small button that says the current choice, and opens a
 * panel to change it. The panel closes on Escape, on a click outside, or
 * when a choice inside it says it is done.
 *
 * Opening it moves keyboard focus to its first control, and closing it by
 * Escape or by a choice returns focus to the chip, so a keyboard user is
 * never left on a control that has gone.
 */
export function ChipMenu({
  name,
  value,
  icon,
  marked = false,
  disabled = false,
  children,
}: {
  /** What the chip chooses, for its accessible name: "Where models run". */
  readonly name: string;
  /** The current choice, shown on the chip. */
  readonly value: string;
  readonly icon: IconName;
  /** A dot on the chip, for something worth a look (a suggested skill). */
  readonly marked?: boolean;
  readonly disabled?: boolean;
  readonly children: (close: () => void) => React.ReactNode;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const holder = useRef<HTMLDivElement>(null);
  const popover = useRef<HTMLDivElement>(null);
  const panel = useId();
  const chipId = useId();
  // A chip that becomes unusable (a task started) also puts its panel away,
  // and it stays away once the chip is usable again.
  const [wasDisabled, setWasDisabled] = useState(disabled);
  if (disabled !== wasDisabled) {
    setWasDisabled(disabled);
    if (disabled) setOpen(false);
  }
  const shown = open && !disabled;

  // Put the panel away and give focus back to the chip that opened it,
  // found by its id: this is handed to the panel's content while rendering.
  const closeToChip = (): void => {
    setOpen(false);
    document.getElementById(chipId)?.focus();
  };

  useEffect(() => {
    if (!shown) return;
    popover.current?.querySelector<HTMLElement>(FOCUSABLE)?.focus();
  }, [shown]);

  useEffect(() => {
    if (!shown) return undefined;
    const onPointer = (event: MouseEvent): void => {
      if (holder.current !== null && !holder.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        setOpen(false);
        document.getElementById(chipId)?.focus();
      }
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [chipId, shown]);

  return (
    <div className="chip-menu" ref={holder}>
      <button
        id={chipId}
        type="button"
        className="chip"
        aria-label={`${name}: ${value}`}
        aria-expanded={shown}
        aria-controls={shown ? panel : undefined}
        disabled={disabled}
        onClick={() => {
          setOpen((current) => !current);
        }}
      >
        <Icon name={icon} size={15} />
        <span>{value}</span>
        {marked && <span className="chip-dot" aria-hidden="true" />}
        <Icon name="chevronDown" size={13} />
      </button>
      {shown && (
        <div ref={popover} id={panel} className="chip-popover" role="dialog" aria-label={name}>
          {children(closeToChip)}
        </div>
      )}
    </div>
  );
}
