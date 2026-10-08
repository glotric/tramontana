import React from "react";
import ReactDOM from "react-dom/client";

import { App } from "./App.js";
import { applyTheme, rememberedTheme } from "./theme.js";
import "./styles.css";

// Before the first paint, so a chosen theme never flashes the other one.
applyTheme(rememberedTheme());

const container = document.getElementById("root");
if (!container) {
  throw new Error("Missing #root element in index.html");
}

ReactDOM.createRoot(container).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
