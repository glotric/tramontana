#!/usr/bin/env node
/**
 * Generate the committed contract artifacts, or verify they are current.
 *
 * The pipeline is Rust -> JSON Schema -> TypeScript (`docs/DEVELOPMENT.md` §8):
 * `crates/core/src/contracts` is the source of truth, `xtask` emits the schema
 * with schemars, and json-schema-to-typescript produces the types the
 * interface imports. Both outputs are committed so a reviewer can see a
 * contract change as a diff.
 *
 *   generate  write schema and TypeScript in place
 *   check     regenerate into a temporary directory and fail on any difference
 */
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { compile } from "json-schema-to-typescript";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const SCHEMA_FILE = "tramontana-contracts.schema.json";
const SCHEMA_DIR = join(root, "packages/shared/schema");
const TS_FILE = join(root, "packages/shared/src/generated/contracts.ts");

const BANNER = `/**
 * DO NOT EDIT. Generated from crates/core/src/contracts by \`pnpm contracts:generate\`.
 *
 * The Rust definitions are the source of truth; \`pnpm contracts:check\` fails
 * when this file is stale.
 */
`;

/** Emit the JSON Schema for the core contracts into `dir`. */
function emitSchema(dir) {
  execFileSync("cargo", ["run", "--quiet", "-p", "xtask", "--", "emit-schema", dir], {
    cwd: root,
    stdio: ["ignore", "pipe", "inherit"],
  });
  return readFileSync(join(dir, SCHEMA_FILE), "utf8");
}

/**
 * Convert the schema document to TypeScript.
 *
 * The document is a container of `$defs` with no root type of its own, so each
 * definition is compiled separately and the results are concatenated. That
 * keeps every contract a named exported type rather than a member of a
 * synthetic wrapper interface.
 */
async function schemaToTypeScript(schemaJson) {
  const document = JSON.parse(schemaJson);
  const defs = document.$defs ?? {};
  const names = Object.keys(defs).sort();

  const parts = [];
  for (const name of names) {
    // Give each definition the sibling `$defs` so internal `$ref`s resolve,
    // and let the compiler skip re-declaring types emitted by an earlier pass.
    const single = { ...defs[name], $defs: defs, title: name };
    const ts = await compile(single, name, {
      bannerComment: "",
      additionalProperties: false,
      declareExternallyReferenced: false,
      style: { singleQuote: false },
    });
    parts.push(ts.trim());
  }

  const seen = new Set();
  const deduped = [];
  for (const part of parts) {
    for (const block of part.split(/\n{2,}/)) {
      const key = /export (?:type|interface) (\w+)/.exec(block)?.[1] ?? block;
      if (seen.has(key)) continue;
      seen.add(key);
      deduped.push(block);
    }
  }

  return `${BANNER}\n${collapseNumberedAliases(deduped.join("\n\n"), seen)}\n`;
}

/**
 * Collapse `Name1`, `Name2` back onto `Name`.
 *
 * Each definition is compiled on its own, so when two of them reference the
 * same shared type the generator emits `ErrorEnvelope` for the first and
 * `ErrorEnvelope1` for the second. Deduplicating by name then drops the second
 * declaration and leaves a reference to a type that does not exist — which
 * type-checks as `any` in some positions and fails outright in others.
 *
 * Rewriting the suffixed references onto the surviving declaration is safe
 * precisely because they came from one schema definition.
 */
function collapseNumberedAliases(source, declared) {
  let output = source;
  const suffixed = new Set(source.match(/\b[A-Z][A-Za-z0-9]*?[0-9]+\b/g) ?? []);

  for (const alias of suffixed) {
    const base = alias.replace(/[0-9]+$/, "");
    if (!declared.has(base)) continue;
    // Drop a duplicate declaration of the alias if one survived, then point
    // every remaining reference at the base type.
    output = output.replace(
      new RegExp(`export (?:type|interface) ${alias}\\b[\\s\\S]*?\\n}\\n`, "g"),
      "",
    );
    output = output.replace(new RegExp(`\\b${alias}\\b`, "g"), base);
  }
  return output;
}

async function generate(schemaDir, tsPath) {
  const schemaJson = emitSchema(schemaDir);
  const ts = await schemaToTypeScript(schemaJson);
  writeFileSync(tsPath, ts, "utf8");
  return { schemaJson, ts };
}

const mode = process.argv[2] ?? "generate";

if (mode === "generate") {
  await generate(SCHEMA_DIR, TS_FILE);
  console.log(`contracts: wrote ${SCHEMA_FILE} and ${TS_FILE.replace(`${root}/`, "")}`);
} else if (mode === "check") {
  const temp = mkdtempSync(join(tmpdir(), "tramontana-contracts-"));
  try {
    const schemaJson = emitSchema(temp);
    const ts = await schemaToTypeScript(schemaJson);

    const stale = [];
    if (schemaJson !== readFileSync(join(SCHEMA_DIR, SCHEMA_FILE), "utf8")) stale.push(SCHEMA_FILE);
    if (ts !== readFileSync(TS_FILE, "utf8")) stale.push("src/generated/contracts.ts");

    if (stale.length > 0) {
      console.error(
        `error: committed contract artifacts are stale: ${stale.join(", ")}\n` +
          "Run `pnpm contracts:generate` and commit the result.",
      );
      process.exit(1);
    }
    console.log("contracts: committed artifacts match crates/core/src/contracts");
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
} else {
  console.error(`usage: contracts.mjs [generate|check]`);
  process.exit(2);
}
