#!/usr/bin/env node
/**
 * Drive the real desktop window: the built application, its webview and its
 * IPC, through `tauri-driver` (ADR 0041).
 *
 * `tauri-driver` speaks WebDriver on Linux (WebKitWebDriver) and Windows
 * (Edge's driver); macOS has no driver for its webview, so this does not run
 * there, and `apps/desktop/src-tauri/src/scenario.rs` covers what it can below
 * the webview (ADR 0037).
 *
 * The client is WebDriver's HTTP protocol over `fetch`, a few calls wide, so
 * no browser-automation package is added for it.
 *
 * What it proves, step by step, is printed as it goes. It needs:
 *
 *   TRAMONTANA_APP   the application built by `tauri build --debug --no-bundle`
 *   TRAMONTANA_CLI   the CLI, to seed a profile the way a person would
 *   TAURI_DRIVER     `tauri-driver`, defaulting to the one on PATH
 */
/* eslint-disable no-restricted-globals -- WebDriver is HTTP to the local
   driver on 127.0.0.1, in test tooling that ships nothing; the rule keeps the
   product's network access behind core-owned adapters. */
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const APP = process.env.TRAMONTANA_APP;
const CLI = process.env.TRAMONTANA_CLI;
const DRIVER = process.env.TAURI_DRIVER ?? "tauri-driver";
const PORT = 4444;
const BASE = `http://127.0.0.1:${PORT}`;
const PATIENCE_MS = 60_000;

if (!APP || !CLI) {
  console.error("window: set TRAMONTANA_APP and TRAMONTANA_CLI");
  process.exit(2);
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** One WebDriver command. Throws with the driver's own message on failure. */
async function command(method, path, body) {
  const response = await fetch(`${BASE}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(`${method} ${path}: ${JSON.stringify(payload.value ?? payload)}`);
  }
  return payload.value;
}

/** Wait until `check` returns something truthy, or fail with `what`. */
async function until(what, check) {
  const deadline = Date.now() + PATIENCE_MS;
  let last;
  while (Date.now() < deadline) {
    try {
      const found = await check();
      if (found) return found;
    } catch (error) {
      last = error;
    }
    await sleep(200);
  }
  throw new Error(`timed out waiting for ${what}${last ? `: ${last.message}` : ""}`);
}

/** A fixture project, as a person might have: a committed Git repository. */
function fixture() {
  const home = mkdtempSync(join(tmpdir(), "tramontana-window-"));
  const root = join(home, "repository");
  mkdirSync(join(root, ".tramontana"), { recursive: true });
  writeFileSync(join(root, "greeting.txt"), "Hello, world\n");
  writeFileSync(
    join(root, ".tramontana/project.md"),
    "---\nid: fixture\nrecipes:\n  test: [sh, check.sh]\n---\n# Fixture\n",
  );
  const git = (...args) =>
    execFileSync(
      "git",
      ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", ...args],
      { cwd: root, stdio: "ignore" },
    );
  git("init", "--quiet");
  git("add", ".");
  git("commit", "--quiet", "-m", "fixture");
  return { root, profile: join(home, "profile.sqlite") };
}

const steps = [];
function step(text) {
  steps.push(text);
  console.log(`window: ${text}`);
}

async function main() {
  const { root, profile } = fixture();
  // Opened through the CLI: the one thing the window cannot be driven to do
  // is choose a folder in the native picker, which is the point of it.
  execFileSync(CLI, ["project", "open", root, "--profile", profile], { stdio: "ignore" });

  const driver = spawn(DRIVER, ["--port", String(PORT)], {
    env: { ...process.env, TRAMONTANA_PROFILE: profile },
    stdio: ["ignore", "inherit", "inherit"],
  });
  let session;
  try {
    await until("tauri-driver to listen", async () => (await fetch(`${BASE}/status`)).ok);
    const created = await command("POST", "/session", {
      capabilities: { alwaysMatch: { "tauri:options": { application: APP } } },
    });
    session = created.sessionId;
    const s = `/session/${session}`;

    const find = async (xpath) =>
      Object.values(await command("POST", `${s}/element`, { using: "xpath", value: xpath }))[0];
    const text = async (xpath) => command("GET", `${s}/element/${await find(xpath)}/text`);
    const click = async (xpath) => command("POST", `${s}/element/${await find(xpath)}/click`, {});
    const type = async (xpath, value) => {
      const element = await find(xpath);
      await command("POST", `${s}/element/${element}/clear`, {});
      await command("POST", `${s}/element/${element}/value`, { text: value });
    };
    const button = (name) => `//button[normalize-space()=${JSON.stringify(name)}]`;
    const shows = (xpath, wanted) =>
      until(`${JSON.stringify(wanted)} in ${xpath}`, async () =>
        (await text(xpath)).includes(wanted),
      );

    // 1. The window reaches the core over real IPC, and the versions agree.
    await shows("//footer", "contracts v1");
    step("the window reached the core over IPC, and the contract versions agree");

    // 2. The profile the CLI wrote is the one the window reads. Projects are
    // Agent mode's; the window opens in Chat (ADR 0057). Agent opens on its
    // home, with the most recent project ready in the composer.
    await click(button("Agent"));
    await until("the seeded project", () => find(button("repository")));
    await until("the project in the composer", () =>
      find("//button[@aria-label='Project: repository']"),
    );
    step("the project the CLI opened is listed, and ready for a task");

    // 3. A task started from the home screen, in a conversation of its own
    // named after the goal.
    await type("//textarea[@id='goal']", "List the project");
    await click("//button[@aria-label='Start task']");
    await until("the new conversation", () => find("//h1[normalize-space()='List the project']"));
    await shows("//*[@aria-label='Task summary']//dd[@data-state]", "completed");
    const cards = await command("POST", `${s}/elements`, {
      using: "xpath",
      value: "//ol[@aria-label='Task activity']/li",
    });
    if (cards.length === 0) throw new Error("the task finished with no activity cards");
    step(`a task started in the window ran to completion, with ${cards.length} activity cards`);

    // 4. A reload keeps the record: it comes back from the core, not memory.
    await command("POST", `${s}/refresh`, {});
    await until("the conversation after a reload", () => find(button("List the project")));
    await click(button("List the project"));
    await shows("//*[@aria-label='Task summary']//dd[@data-state]", "completed");
    step("after a reload the task and its state came back from the record");

    // 5. The settings views open, and say what is and is not configured.
    // They sit under More in the sidebar.
    await click(button("More"));
    await click(button("Models and credentials"));
    await shows("//section[contains(@class,'models-view')]", "No model endpoints are configured");
    await click(button("Back to work"));
    await click(button("Apps"));
    await shows("//section[contains(@class,'models-view')]", "No Apps are configured");
    await click(button("Back to work"));
    step("the Models and Apps views open, and say nothing is configured");

    console.log(`window: PASS (${steps.length} steps)`);
  } finally {
    if (session !== undefined) {
      await command("DELETE", `/session/${session}`).catch(() => undefined);
    }
    driver.kill();
  }
}

main().catch((error) => {
  console.error(`window: FAIL after ${steps.length} step(s): ${error.message}`);
  process.exit(1);
});
