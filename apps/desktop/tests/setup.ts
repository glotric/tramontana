import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// jsdom lays nothing out, so it has no scrolling to do; the chat scrolls to
// its newest turn, which a real WebView does.
Element.prototype.scrollIntoView = function scrollIntoView(): void {
  // Nothing to scroll.
};

afterEach(() => {
  cleanup();
  localStorage.clear();
});
