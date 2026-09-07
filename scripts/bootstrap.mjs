#!/usr/bin/env node
/**
 * Validate the local toolchain and generate repository-local artifacts.
 *
 * Deliberately does not install anything: no models, no external executables,
 * no package managers (`docs/DEVELOPMENT.md` §4). It reports what is missing
 * and how to fix it, then generates the committed contract artifacts so a
 * clean checkout reaches a verifiable state.
 */
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const problems = [];
const notes = [];

function read(relativePath) {
  return readFileSync(join(root, relativePath), "utf8");
}

function tryRun(command, args) {
  try {
    return execFileSync(command, args, {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  } catch {
    return null;
  }
}

// --- Node -------------------------------------------------------------------
// Node 25 removed the bundled Corepack that activates the pinned pnpm, so the
// supported range is deliberately narrow rather than ">= LTS". See ADR 0002.
const wantedNode = read(".node-version").trim();
const wantedNodeMajor = wantedNode.split(".")[0];
const actualNodeMajor = process.versions.node.split(".")[0];
if (actualNodeMajor !== wantedNodeMajor) {
  problems.push(
    `Node ${wantedNodeMajor}.x required (.node-version pins ${wantedNode}), found ${process.versions.node}.\n` +
      `    Install it, or run through a version manager that reads .node-version.\n` +
      `    Homebrew: brew install node@${wantedNodeMajor} && export PATH="$(brew --prefix node@${wantedNodeMajor})/bin:$PATH"`,
  );
}

// --- pnpm -------------------------------------------------------------------
const pkg = JSON.parse(read("package.json"));
const wantedPnpm = pkg.packageManager?.replace(/^pnpm@/, "");
const actualPnpm = tryRun("pnpm", ["--version"]);
if (!actualPnpm) {
  const hasCorepack = tryRun("corepack", ["--version"]) !== null;
  problems.push(
    hasCorepack
      ? `pnpm ${wantedPnpm} not found. Enable it with: corepack enable pnpm`
      : `pnpm ${wantedPnpm} not found, and Corepack is not available to activate it.\n` +
          `    Corepack ships with Node ${wantedNodeMajor} but was removed in Node 25, so this\n` +
          `    usually means Node is newer than the pin. Install Node ${wantedNodeMajor}.x, then run: corepack enable pnpm`,
  );
} else if (actualPnpm !== wantedPnpm) {
  notes.push(
    `pnpm ${actualPnpm} is active; package.json pins ${wantedPnpm}. Corepack should reconcile this on the next pnpm run.`,
  );
}

// --- Rust -------------------------------------------------------------------
const toolchain = read("rust-toolchain.toml");
const wantedRust = /channel\s*=\s*"([^"]+)"/.exec(toolchain)?.[1];
const rustcVersion = tryRun("rustc", ["--version"]);
if (!rustcVersion) {
  problems.push("rustc not found. Install Rust through rustup: https://rustup.rs");
} else if (wantedRust && !rustcVersion.includes(wantedRust)) {
  notes.push(
    `rustc reports "${rustcVersion}"; rust-toolchain.toml pins ${wantedRust}. rustup selects the pinned version inside this repository.`,
  );
}
// The rustup component and the cargo subcommand are not the same word: the
// `rustfmt` component is invoked as `cargo fmt`.
for (const [component, subcommand] of [
  ["rustfmt", "fmt"],
  ["clippy", "clippy"],
]) {
  if (!tryRun("cargo", [subcommand, "--version"])) {
    problems.push(
      `cargo ${subcommand} not available. Install it with: rustup component add ${component}`,
    );
  }
}

// --- Platform prerequisites -------------------------------------------------
// Tauri's native build needs a platform toolchain and a system WebView.
// See https://v2.tauri.app/start/prerequisites/ for the authoritative list.
if (process.platform === "darwin") {
  if (!tryRun("xcode-select", ["-p"])) {
    problems.push("Xcode Command Line Tools not installed. Run: xcode-select --install");
  }
} else if (process.platform === "linux") {
  const missing = ["webkit2gtk-4.1", "libsoup-3.0"].filter(
    (lib) => !tryRun("pkg-config", ["--exists", lib]),
  );
  if (missing.length > 0) {
    problems.push(
      `Missing Tauri system libraries: ${missing.join(", ")}.\n` +
        "    Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev libssl-dev patchelf",
    );
  }
} else if (process.platform === "win32") {
  notes.push(
    "Windows needs the Visual Studio C++ build tools and WebView2. See https://v2.tauri.app/start/prerequisites/",
  );
}

// --- Report -----------------------------------------------------------------
for (const note of notes) console.log(`note: ${note}`);
if (problems.length > 0) {
  console.error("\nBootstrap found prerequisites that need attention:\n");
  for (const problem of problems) console.error(`  - ${problem}\n`);
  process.exit(1);
}

console.log("Toolchain OK. Generating contract artifacts...");
execFileSync("node", [join(root, "scripts/contracts.mjs"), "generate"], {
  stdio: "inherit",
  cwd: root,
});
console.log("\nBootstrap complete. Next: pnpm check");
