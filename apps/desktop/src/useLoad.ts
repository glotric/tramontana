import { useEffect, useState, type DependencyList } from "react";

import { restartSupported } from "./core.js";

/**
 * Start `load` whenever `deps` change and hand its outcome to `settle` or
 * `fail`, unless the effect was torn down first (unmounted, or superseded by a
 * newer read). `load` returns undefined to skip a read.
 */
export function useLoad<T>(
  load: () => Promise<T> | undefined,
  deps: DependencyList,
  settle: (value: T) => void,
  fail: (error: unknown) => void = () => undefined,
): void {
  useEffect(() => {
    let active = true;
    void load()?.then(
      (value) => {
        if (active) settle(value);
      },
      (error: unknown) => {
        if (active) fail(error);
      },
    );
    return () => {
      active = false;
    };
    // The caller's deps decide when to read again, as with useEffect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

/** Whether this build can restart itself; undefined until known, false if the check fails. */
export function useRestartable(): boolean | undefined {
  const [restartable, setRestartable] = useState<boolean | undefined>();
  useLoad(restartSupported, [], setRestartable, () => {
    setRestartable(false);
  });
  return restartable;
}
