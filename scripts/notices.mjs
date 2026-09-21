#!/usr/bin/env node
/**
 * Generate the third-party notices shipped with Tramontana, or verify they are
 * current.
 *
 * Covers what is redistributed, not what is used to build: the Rust crates
 * compiled into the desktop application and the CLI (normal and build
 * dependencies, every platform), and the npm packages bundled into the
 * desktop's interface. Each is listed with its version, its declared licence
 * and where it came from, followed by every distinct licence text, once, with
 * the packages it covers.
 *
 * A package that ships no licence file still has obligations. Its manifest's
 * licence is honored with a standard text: the first of MIT, Apache-2.0,
 * BSD-3-Clause, MPL-2.0 or Zlib its expression allows, filled in with its
 * authors where the text names a holder. That substitution is listed as such,
 * never silently.
 *
 *   generate  write THIRD_PARTY_NOTICES.txt in place
 *   check     fail when the committed file differs from a fresh one
 */
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const OUTPUT = join(root, "THIRD_PARTY_NOTICES.txt");
const SHIPPED_CRATES = ["tramontana-desktop", "tramontana-cli"];
const SHIPPED_NPM = join(root, "apps/desktop");
const LICENCE_FILE = /^(licen[cs]e|copying|notice|unlicense)/i;

/** Byte order, not locale order: the file must come out the same on every machine. */
const byCodePoint = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/** A licence file's text, with line endings and trailing space normalized. */
function readText(path) {
  return readFileSync(path, "utf8")
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")
    .trim();
}

/** Licence files at a package's root, including a REUSE-style `LICENSES/` folder. */
function licenceFiles(directory) {
  return readdirSync(directory, { withFileTypes: true })
    .filter((entry) => LICENCE_FILE.test(entry.name))
    .sort((a, b) => byCodePoint(a.name, b.name))
    .flatMap((entry) => {
      const path = join(directory, entry.name);
      if (!entry.isDirectory()) return [path];
      return readdirSync(path, { withFileTypes: true })
        .filter((inner) => inner.isFile())
        .map((inner) => join(path, inner.name))
        .sort(byCodePoint);
    });
}

/** The Rust crates the shipped binaries are built from. */
function rustPackages() {
  const metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--locked"], {
      cwd: root,
      maxBuffer: 256 * 1024 * 1024,
      stdio: ["ignore", "pipe", "inherit"],
    }).toString(),
  );
  const packages = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const members = new Set(metadata.workspace_members);
  const stack = metadata.workspace_members.filter((id) =>
    SHIPPED_CRATES.includes(packages.get(id).name),
  );
  const seen = new Set();
  while (stack.length > 0) {
    const id = stack.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const dep of nodes.get(id).deps) {
      // Dev-dependencies build tests, not what is shipped.
      if (dep.dep_kinds.some((kind) => kind.kind === null || kind.kind === "build")) {
        stack.push(dep.pkg);
      }
    }
  }
  return [...seen]
    .filter((id) => !members.has(id))
    .map((id) => packages.get(id))
    .map((pkg) => ({
      ecosystem: "crate",
      name: pkg.name,
      version: pkg.version,
      licence: pkg.license ?? `see ${pkg.license_file ?? "the package"}`,
      source: pkg.repository ?? `https://crates.io/crates/${pkg.name}`,
      authors: pkg.authors ?? [],
      files: licenceFiles(dirname(pkg.manifest_path)),
    }));
}

/** The npm packages bundled into the desktop's interface, as pnpm laid them out. */
function npmPackages() {
  const found = new Map();
  const visit = (directory) => {
    const manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
    const key = `${manifest.name}@${manifest.version}`;
    if (found.has(key)) return;
    const firstParty = manifest.name.startsWith("@tramontana/");
    if (!firstParty) {
      found.set(key, {
        ecosystem: "npm",
        name: manifest.name,
        version: manifest.version,
        licence: manifest.license ?? "unspecified",
        source:
          typeof manifest.repository === "string"
            ? manifest.repository
            : (manifest.repository?.url ?? `https://www.npmjs.com/package/${manifest.name}`),
        authors: manifest.author ? [String(manifest.author.name ?? manifest.author)] : [],
        files: licenceFiles(directory),
      });
    }
    for (const dep of Object.keys(manifest.dependencies ?? {})) {
      // pnpm places a package's dependencies beside it, and a workspace
      // package's in its own node_modules.
      const candidates = [
        join(directory, "node_modules", dep),
        join(dirname(directory), dep),
        join(dirname(dirname(directory)), dep),
      ];
      const next = candidates.find((candidate) => existsSync(join(candidate, "package.json")));
      if (next === undefined) throw new Error(`cannot find ${dep}, needed by ${key}`);
      visit(realpathSync(next));
    }
  };
  visit(SHIPPED_NPM);
  return [...found.values()];
}

/** The SPDX identifiers an expression allows, in the order they appear. */
function identifiers(expression) {
  return expression
    .replace(/[()]/g, " ")
    .split(/\s+|\//)
    .filter((word) => word !== "" && !["OR", "AND", "WITH"].includes(word));
}

const PREFERRED = ["MIT", "Apache-2.0", "BSD-3-Clause", "MPL-2.0", "Zlib"];

function mit(holder) {
  return `MIT License

Copyright (c) ${holder}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`;
}

function bsd3(holder) {
  return `BSD 3-Clause License

Copyright (c) ${holder}

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.`;
}

/**
 * A standard text for `id`: templated where it names a holder, and otherwise
 * taken from a shipped package that declares that licence alone and ships
 * its text, so nothing here is typed from memory that could be copied.
 */
function standardText(id, pkg, all) {
  const holder = pkg.authors.length > 0 ? pkg.authors.join(", ") : `the ${pkg.name} authors`;
  if (id === "MIT") return mit(holder);
  if (id === "BSD-3-Clause") return bsd3(holder);
  const reference = all.find(
    (other) =>
      identifiers(other.licence).length === 1 &&
      identifiers(other.licence)[0] === id &&
      other.files.length === 1,
  );
  const byName = {
    "Apache-2.0": /apache/i,
  };
  if (reference) return readText(reference.files[0]);
  const named = all
    .flatMap((other) => other.files)
    .find((file) => byName[id]?.test(file.split(/[\\/]/).pop()));
  if (named) return readText(named);
  throw new Error(`no standard text for ${id}, needed by ${pkg.name} ${pkg.version}`);
}

function render(packages) {
  packages.sort(
    (a, b) =>
      byCodePoint(a.ecosystem, b.ecosystem) ||
      byCodePoint(a.name, b.name) ||
      byCodePoint(a.version, b.version),
  );

  // Every distinct text once, numbered in order of first use.
  const texts = new Map();
  const applies = new Map();
  const substituted = [];
  const cite = (text, label) => {
    if (!texts.has(text)) {
      texts.set(text, texts.size + 1);
      applies.set(text, []);
    }
    applies.get(text).push(label);
    return texts.get(text);
  };

  const rows = packages.map((pkg) => {
    const label = `${pkg.name} ${pkg.version}`;
    let refs;
    if (pkg.files.length > 0) {
      refs = pkg.files.map((file) => cite(readText(file), label));
    } else {
      const ids = identifiers(pkg.licence);
      const chosen = PREFERRED.find((id) => ids.includes(id));
      if (chosen === undefined) {
        throw new Error(`${label} ships no licence text and declares ${pkg.licence}`);
      }
      substituted.push(`${label} (${pkg.licence}): standard ${chosen} text supplied`);
      refs = [cite(standardText(chosen, pkg, packages), label)];
    }
    return `${pkg.ecosystem.padEnd(5)} ${label}\n      Licence: ${pkg.licence}\n      Source: ${pkg.source}\n      Texts: ${refs.map((ref) => `[${ref}]`).join(" ")}`;
  });

  const rule = "=".repeat(78);
  return `${[
    "THIRD-PARTY NOTICES",
    "",
    "DO NOT EDIT. Generated by `pnpm notices:generate` from Cargo.lock and the",
    "pnpm lockfile; `pnpm notices:check` fails when this file is stale.",
    "",
    "Tramontana's desktop application and command-line tool include the",
    `following ${packages.length} third-party packages. Each is listed with the licence its`,
    "manifest declares and the texts that apply to it; every distinct text",
    "appears once, at the end.",
    "",
    rule,
    "PACKAGES",
    rule,
    "",
    rows.join("\n\n"),
    "",
    rule,
    "PACKAGES THAT SHIP NO LICENCE TEXT",
    rule,
    "",
    "These packages declare a licence but include no text of it. The standard",
    "text of the first licence their expression allows, among MIT, Apache-2.0,",
    "BSD-3-Clause, MPL-2.0 and Zlib, is supplied in its place, naming their",
    "listed authors where the text names a holder.",
    "",
    ...substituted,
    "",
    rule,
    "LICENCE TEXTS",
    rule,
    ...[...texts.entries()].flatMap(([text, number]) => [
      "",
      `[${number}] Applies to: ${applies.get(text).join(", ")}`,
      "-".repeat(78),
      text,
    ]),
  ].join("\n")}\n`;
}

function generate() {
  return render([...rustPackages(), ...npmPackages()]);
}

const mode = process.argv[2];
if (mode === "generate") {
  writeFileSync(OUTPUT, generate());
  console.log(`notices: wrote ${OUTPUT}`);
} else if (mode === "check") {
  const fresh = generate();
  // A Windows checkout may have converted line endings; the content is what
  // is compared.
  const committed = existsSync(OUTPUT) ? readFileSync(OUTPUT, "utf8").replace(/\r\n/g, "\n") : "";
  if (fresh !== committed) {
    console.error("notices: THIRD_PARTY_NOTICES.txt is stale; run `pnpm notices:generate`");
    process.exit(1);
  }
  console.log("notices: THIRD_PARTY_NOTICES.txt matches the lockfiles");
} else {
  console.error("usage: node scripts/notices.mjs <generate|check>");
  process.exit(2);
}
