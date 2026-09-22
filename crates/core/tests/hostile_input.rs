//! Hostile input to the parsers that read what Tramontana did not write.
//!
//! `TESTING_STRATEGY.md` §5 and M12's "malicious-input testing". A project's
//! Markdown, an App's tool schemas and arguments are written by someone else,
//! and a parser that panics on them takes the application down with it. Each
//! test here takes valid input, damages it thousands of ways with a seeded
//! generator — deterministic, so a failure reproduces — and requires an
//! answer, never a panic. What the answer is, each parser's own tests say.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use tramontana_core::config::{RawSource, resolve, split_front_matter};
use tramontana_core::contracts::{ConfigurationKind, Timestamp};
use tramontana_core::services::ToolInputSchema;

/// xorshift64*: small, seeded, and the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % (bound.max(1) as u64)).unwrap()
    }
}

/// Characters that mean something to YAML, JSON or Markdown, and a few that
/// mean something to nobody.
const SPICE: &[&str] = &[
    "---",
    "\n",
    ":",
    "- ",
    "[",
    "]",
    "{",
    "}",
    "\"",
    "'",
    "&a",
    "*a",
    "!!",
    "?",
    "|",
    ">",
    "#",
    "\t",
    "\\",
    "\u{0}",
    "\u{feff}",
    "é",
    "𝕏",
    "\r\n",
    "recipes:",
    "  test: [sh]",
    "id: ",
];

/// One damaged copy of `input`: bytes cut, repeated, swapped or seasoned.
fn damage(rng: &mut Rng, input: &str) -> String {
    let mut text: Vec<char> = input.chars().collect();
    for _ in 0..=rng.below(4) {
        let at = rng.below(text.len() + 1);
        match rng.below(5) {
            0 if !text.is_empty() => {
                let end = (at + rng.below(20)).min(text.len());
                text.drain(at.min(end)..end);
            }
            1 => {
                let insert: Vec<char> = SPICE[rng.below(SPICE.len())].chars().collect();
                text.splice(at..at, insert);
            }
            2 if !text.is_empty() => {
                let from = rng.below(text.len());
                let end = (from + rng.below(40)).min(text.len());
                let copy: Vec<char> = text[from..end].to_vec();
                text.splice(at..at, copy);
            }
            3 => text.truncate(at),
            _ => {
                let repeat: String = SPICE[rng.below(SPICE.len())].repeat(1 + rng.below(300));
                text.splice(at..at, repeat.chars());
            }
        }
    }
    text.into_iter().collect()
}

const PROJECT: &str = "---\nid: fixture\nname: Fixture\ndescription: A project.\ntags: [a, b]\nrecipes:\n  test: [sh, check.sh]\n  lint: [cargo, clippy, \"--\", \"-D\", warnings]\n---\n# Fixture\n\nThe body.\n";
const RULE: &str = "---\nid: style\ndescription: House style.\nscopes: [\"src/**\"]\npriority: 3\ncapabilities: [fs.read]\n---\nWrite plainly.\n";

#[test]
fn damaged_front_matter_is_read_or_refused_never_a_crash() {
    let mut rng = Rng(0x7a3c_91e5_d2b4_0f61);
    for round in 0..4000 {
        let document = damage(&mut rng, if round % 2 == 0 { PROJECT } else { RULE });
        let _ = split_front_matter(&document);
    }
}

#[test]
fn nesting_past_any_sensible_depth_is_refused_not_a_stack_overflow() {
    // The parser builds the whole tree before Tramontana can measure it, so
    // its own limit is what stands between a hostile file and the stack. Run
    // on a thread with the stack the application's worker threads get.
    let refused = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            [1_000_usize, 30_000].iter().all(|&depth| {
                let block = format!(
                    "---\nid: x\nnested: {}{}\n---\n",
                    "[".repeat(depth),
                    "]".repeat(depth)
                );
                split_front_matter(&block).is_err()
            })
        })
        .unwrap()
        .join()
        .expect("a deeply nested block crashed the parser");
    assert!(refused);
}

#[test]
fn a_damaged_project_configuration_resolves_to_diagnostics_never_a_crash() {
    let mut rng = Rng(0x1f2e_3d4c_5b6a_7988);
    let at = Timestamp::new("2026-01-01T00:00:00.000Z");
    for _ in 0..1500 {
        let sources = vec![
            RawSource {
                relative_path: ".tramontana/project.md".to_owned(),
                kind: ConfigurationKind::ProjectOverview,
                content: damage(&mut rng, PROJECT).into_bytes(),
            },
            RawSource {
                relative_path: ".tramontana/rules/style.md".to_owned(),
                kind: ConfigurationKind::Rule,
                content: damage(&mut rng, RULE).into_bytes(),
            },
            RawSource {
                relative_path: "AGENTS.md".to_owned(),
                kind: ConfigurationKind::RootAgents,
                // Not even text, sometimes.
                content: damage(&mut rng, "# Agents\n\nBe careful.\n")
                    .into_bytes()
                    .into_iter()
                    .map(|byte| if rng.below(50) == 0 { 0xff } else { byte })
                    .collect(),
            },
        ];
        let configuration = resolve(sources, at.clone());
        assert_eq!(configuration.sources.len(), 3);
    }
}

const SCHEMA: &str = r#"{"type":"object","description":"Read a file.","properties":{"path":{"type":"string","description":"Where."},"limit":{"type":"integer"},"tags":{"type":"array","items":{"type":"string"}},"mode":{"type":"string","enum":["a","b"]}},"required":["path"],"additionalProperties":false}"#;
const ARGUMENTS: &str = r#"{"path":"src/main.rs","limit":10,"tags":["x","y"],"mode":"a"}"#;

#[test]
fn a_hostile_tool_schema_or_arguments_are_judged_never_a_crash() {
    // What an App sends as its tool's input schema, and what a model sends as
    // the arguments, both damaged; only the ones that still parse as JSON
    // reach the compiler and the validator, as on the wire.
    let mut rng = Rng(0x0dd_ba11_cafe_f00d);
    // The undamaged schema is one the restricted compiler accepts.
    ToolInputSchema::compile(&serde_json::from_str(SCHEMA).unwrap()).unwrap();
    let mut compiled = 0;
    for _ in 0..4000 {
        let Ok(schema) = serde_json::from_str(&damage(&mut rng, SCHEMA)) else {
            continue;
        };
        let Ok(schema) = ToolInputSchema::compile(&schema) else {
            continue;
        };
        compiled += 1;
        for _ in 0..4 {
            if let Ok(arguments) = serde_json::from_str(&damage(&mut rng, ARGUMENTS)) {
                let _ = schema.validate(&arguments);
            }
        }
    }
    // The damage leaves enough valid schemas for the validator to be tried.
    assert!(compiled > 100, "only {compiled} damaged schemas compiled");
}
