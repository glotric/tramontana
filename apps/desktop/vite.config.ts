/// <reference types="vitest/config" />
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri drives this server, so the port is fixed and a clash must fail loudly
// rather than silently landing somewhere the shell will not load.
const DEV_PORT = 1420;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: DEV_PORT,
    strictPort: true,
    watch: {
      // Rust sources are rebuilt by the Tauri CLI, not by Vite.
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Tauri ships a known WebView per platform, so there is no legacy target.
    target: "es2023",
    sourcemap: process.env["TAURI_ENV_DEBUG"] === "true",
  },
  test: {
    environment: "jsdom",
    globals: false,
    // Every mock returns to its own implementation before each test.
    mockReset: true,
    setupFiles: ["./tests/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}", "tests/**/*.test.{ts,tsx}"],
  },
});
