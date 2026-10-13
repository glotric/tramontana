import { useRef } from "react";

export interface SegmentedOption<T extends string> {
  readonly value: T;
  readonly label: React.ReactNode;
}

/**
 * One of several, said in words. `radios` is a choice (a setting), `tabs`
 * switches a panel the caller renders with `role="tabpanel"` and
 * `aria-labelledby={segmentedTabId(idPrefix, value)}`.
 *
 * Both follow the WAI-ARIA pattern: only the chosen segment is in the Tab
 * order, and the arrow keys, Home and End move the choice.
 */
export function Segmented<T extends string>({
  kind,
  label,
  value,
  options,
  onChange,
  className,
  idPrefix,
  panelId,
}: {
  readonly kind: "radios" | "tabs";
  readonly label: string;
  readonly value: T;
  readonly options: readonly SegmentedOption<T>[];
  readonly onChange: (value: T) => void;
  readonly className?: string;
  /** Tabs only: the prefix of each tab's id. */
  readonly idPrefix?: string;
  /** Tabs only: the id of the panel the tabs control. */
  readonly panelId?: string;
}): React.JSX.Element {
  const refs = useRef(new Map<T, HTMLButtonElement>());
  const tabs = kind === "tabs";

  const move = (to: number): void => {
    const next = options[(to + options.length) % options.length];
    if (next === undefined) return;
    onChange(next.value);
    refs.current.get(next.value)?.focus();
  };

  const onKeyDown = (event: React.KeyboardEvent, index: number): void => {
    const step =
      event.key === "ArrowRight" || event.key === "ArrowDown"
        ? index + 1
        : event.key === "ArrowLeft" || event.key === "ArrowUp"
          ? index - 1
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? options.length - 1
              : undefined;
    if (step === undefined) return;
    event.preventDefault();
    move(step);
  };

  return (
    <div className={className} role={tabs ? "tablist" : "radiogroup"} aria-label={label}>
      {options.map((option, index) => {
        const chosen = option.value === value;
        return (
          <button
            key={option.value}
            ref={(element) => {
              if (element === null) refs.current.delete(option.value);
              else refs.current.set(option.value, element);
            }}
            type="button"
            role={tabs ? "tab" : "radio"}
            id={tabs && idPrefix !== undefined ? segmentedTabId(idPrefix, option.value) : undefined}
            aria-selected={tabs ? chosen : undefined}
            aria-checked={tabs ? undefined : chosen}
            aria-controls={tabs ? panelId : undefined}
            tabIndex={chosen ? 0 : -1}
            onClick={() => {
              onChange(option.value);
            }}
            onKeyDown={(event) => {
              onKeyDown(event, index);
            }}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

export function segmentedTabId(idPrefix: string, value: string): string {
  return `${idPrefix}-tab-${value}`;
}
