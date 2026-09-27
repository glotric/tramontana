//! Which of a project's skills look relevant to what the person asked for.
//!
//! `CONFIGURATION.md` §6 says a skill "may be selected by the user, suggested
//! by an agent, or enabled by a workflow", and §5 gives it `suggest_when`
//! keywords for exactly this. Nothing used them until ADR 0054.
//!
//! Two rules hold this apart from selection:
//!
//! - **A suggestion is not a selection.** This says which skills look
//!   relevant. What a task uses is still only what it names (ADR 0053), and
//!   nothing here ticks a box or starts anything.
//! - **The rule is here, not in an application.** Both the window and the
//!   terminal suggest the same skills for the same goal, because neither
//!   owns the matching (`AGENTS.md`, "Architectural boundaries").

use crate::contracts::{ConfigurationKind, EffectiveConfiguration};

/// The skills whose keywords appear in `goal`, by declared id.
///
/// A skill matches when one of its `suggest_when` keywords appears in the
/// goal, ignoring case, bounded by something that is not a letter or digit —
/// so `rust` matches "port it to Rust" and "rust:" but not "trustworthy".
/// A keyword of several words matches as that phrase, with each run of
/// whitespace in either treated as one space.
///
/// Returned in the configuration's own order, strongest precedence first, so
/// two runs over the same project and goal answer identically. A skill that
/// declares no keywords is never suggested: silence is not a match. A
/// disabled skill, and one with no declared `id` for a task to name, are both
/// left out.
#[must_use]
pub fn suggested_skills(configuration: &EffectiveConfiguration, goal: &str) -> Vec<String> {
    let goal = normalize(goal);
    configuration
        .sources
        .iter()
        .filter(|source| source.enabled && source.kind == ConfigurationKind::Skill)
        .filter(|source| {
            source
                .suggest_when
                .iter()
                .any(|keyword| mentions(&goal, &normalize(keyword)))
        })
        .filter_map(|source| source.declared_id.clone())
        .collect()
}

/// Lowercase, with every run of whitespace reduced to one space.
///
/// So a keyword written `release notes` matches a goal that wrapped the two
/// words across a line, and neither side's spacing decides the answer.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `keyword` appears in `goal` bounded by non-alphanumeric characters.
fn mentions(goal: &str, keyword: &str) -> bool {
    if keyword.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(offset) = goal.get(from..).and_then(|rest| rest.find(keyword)) {
        let start = from + offset;
        let end = start + keyword.len();
        let before = goal[..start].chars().next_back();
        let after = goal[end..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return true;
        }
        // Past this occurrence, not past the whole keyword: overlapping
        // occurrences would otherwise be skipped.
        from = start + keyword.chars().next().map_or(1, char::len_utf8);
    }
    false
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::config::{ConfigurationSelection, RawSource, resolve_with_selection};
    use crate::contracts::Timestamp;

    fn project(skills: &[(&str, &str)]) -> EffectiveConfiguration {
        resolve_with_selection(
            skills
                .iter()
                .map(|(id, front_matter)| RawSource {
                    relative_path: format!(".tramontana/skills/{id}.md"),
                    kind: ConfigurationKind::Skill,
                    content: format!("---\nid: {id}\n{front_matter}---\nDo the thing.\n")
                        .into_bytes(),
                })
                .collect(),
            &ConfigurationSelection::default(),
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        )
    }

    #[test]
    fn a_keyword_in_the_goal_suggests_its_skill() {
        let configuration = project(&[
            ("rust-quality", "suggest_when: [rust, cargo]\n"),
            ("release", "suggest_when: [release]\n"),
        ]);

        assert_eq!(
            suggested_skills(&configuration, "Tidy the Rust parser"),
            vec!["rust-quality".to_owned()]
        );
        assert_eq!(
            suggested_skills(&configuration, "cut a release"),
            vec!["release".to_owned()]
        );
    }

    #[test]
    fn a_keyword_inside_a_longer_word_is_not_a_match() {
        let configuration = project(&[("rust-quality", "suggest_when: [rust]\n")]);

        assert!(
            suggested_skills(&configuration, "Make the API trustworthy").is_empty(),
            "a keyword must be a word in the goal, not a fragment of one"
        );
        assert_eq!(
            suggested_skills(&configuration, "rust: fix the parser"),
            vec!["rust-quality".to_owned()],
            "punctuation still bounds a word"
        );
    }

    #[test]
    fn a_skill_that_declares_no_keywords_is_never_suggested() {
        let configuration = project(&[("quiet", "")]);

        assert!(
            suggested_skills(&configuration, "quiet").is_empty(),
            "silence is not a match, even when the goal says its name"
        );
    }

    #[test]
    fn a_disabled_skill_is_not_suggested() {
        let configuration = project(&[("rust-quality", "enabled: false\nsuggest_when: [rust]\n")]);

        assert!(suggested_skills(&configuration, "Tidy the Rust parser").is_empty());
    }

    #[test]
    fn suggestions_keep_the_configuration_s_order() {
        // Two skills, both matching: the answer must not depend on which the
        // filesystem listed first.
        let configuration = project(&[
            ("a-skill", "suggest_when: [parser]\n"),
            ("b-skill", "suggest_when: [parser]\n"),
        ]);

        let suggested = suggested_skills(&configuration, "Fix the parser");
        assert_eq!(suggested, vec!["a-skill".to_owned(), "b-skill".to_owned()]);
        assert_eq!(
            suggested,
            suggested_skills(&configuration, "Fix the parser"),
            "the same project and goal answer identically"
        );
    }

    #[test]
    fn a_phrase_matches_across_whatever_spacing_either_side_used() {
        let configuration = project(&[("release", "suggest_when: [\"release notes\"]\n")]);

        assert_eq!(
            suggested_skills(&configuration, "Write the release\n  notes"),
            vec!["release".to_owned()]
        );
        assert!(
            suggested_skills(&configuration, "Write the release").is_empty(),
            "half a phrase is not the phrase"
        );
    }

    #[test]
    fn suggesting_is_not_selecting() {
        // The suggestion names a skill; nothing about the configuration it
        // was read from changes, and no source becomes active.
        let configuration = project(&[("rust-quality", "suggest_when: [rust]\n")]);

        assert_eq!(
            suggested_skills(&configuration, "Tidy the Rust parser"),
            vec!["rust-quality".to_owned()]
        );
        assert!(
            configuration.sources.iter().all(|source| !source.active),
            "a suggestion must not activate anything"
        );
    }
}
