//! Removing credential-shaped text before it becomes a record.
//!
//! `SECURITY_AND_PERMISSIONS.md` §7: "Redact known secret values and common
//! credential patterns at ingestion and before UI/log/event output. Prefer
//! allowlisted diagnostic fields over regex-only cleanup."
//!
//! Two things follow from that sentence, and both shape this module.
//!
//! **At ingestion, not at output.** Redacting on the way out means the secret
//! was durably stored first, and every consumer that forgets to redact is a
//! leak. Redacting on the way in means the record never held it: the durable
//! event, the context manifest, the checkpoint, and the export bundle are all
//! downstream of one place. It costs the ability to un-redact, which is the
//! point.
//!
//! **Patterns are the backstop, not the mechanism.** A regex over free text
//! cannot be the primary defence — it has false positives, false negatives,
//! and no idea what a secret looks like next year. The primary mechanism in
//! this system is structural: credentials live in the platform store behind
//! `SecretRef` and never enter a serializable type at all (ADR 0008). What is
//! here catches the one case structure cannot: a person pasting a key into a
//! prompt, and a tool reading a file that has one in it.
//!
//! So the rules are deliberately narrow. Each matches a shape that is a
//! credential and is not plausibly anything else — a provider key prefix, an
//! `Authorization` header, a PEM private key block, a URL with a password in
//! it. There is no rule for "a long random-looking string", because a hash, a
//! commit id, a base64 image, and a UUID are all long random-looking strings,
//! and a redactor that eats those makes the record useless while providing
//! false comfort.

use crate::contracts::content_hash;

/// Stable codes for what was removed.
///
/// Recorded on the manifest and the release plan, so "something was redacted"
/// is visible and specific without the record naming what it was.
pub mod reason {
    /// A provider API key, recognized by its issuer's prefix.
    pub const PROVIDER_API_KEY: &str = "redaction.providerApiKey";
    /// An HTTP authorization header carrying a token.
    pub const AUTHORIZATION_HEADER: &str = "redaction.authorizationHeader";
    /// A PEM-encoded private key block.
    pub const PRIVATE_KEY_BLOCK: &str = "redaction.privateKeyBlock";
    /// A password embedded in a URL's userinfo.
    pub const URL_CREDENTIALS: &str = "redaction.urlCredentials";
    /// An assignment whose name says it holds a credential.
    pub const CREDENTIAL_ASSIGNMENT: &str = "redaction.credentialAssignment";
}

/// What replaces a match.
///
/// Fixed rather than length-preserving: a placeholder that matched the
/// original's length would leak the length, and for a key with a known format
/// the length is most of the identification.
pub const PLACEHOLDER: &str = "[redacted]";

/// Text with its credentials removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    /// The text as it may be recorded and sent.
    pub text: String,
    /// Stable codes for what was removed, sorted and deduplicated.
    pub reason_codes: Vec<String>,
}

impl Redacted {
    /// Whether anything was removed.
    #[must_use]
    pub fn changed(&self) -> bool {
        !self.reason_codes.is_empty()
    }
}

/// Provider key prefixes that identify a credential on sight.
///
/// A prefix plus a run of key characters, which is what makes this narrow
/// enough to be safe: `sk-` alone would match ordinary prose.
const KEY_PREFIXES: &[&str] = &[
    "sk-",  // OpenAI and compatible, and Anthropic's `sk-ant-`
    "ghp_", // GitHub personal access token
    "gho_", // GitHub OAuth
    "ghs_", // GitHub server-to-server
    "github_pat_",
    "xoxb-",  // Slack bot
    "xoxp-",  // Slack user
    "AKIA",   // AWS access key id
    "ASIA",   // AWS temporary access key id
    "AIza",   // Google API key
    "glpat-", // GitLab personal access token
];

/// Names whose assigned value is a credential.
const CREDENTIAL_NAMES: &[&str] = &[
    "api_key",
    "apikey",
    "access_token",
    "auth_token",
    "secret_key",
    "secret",
    "password",
    "passwd",
    "private_key",
    "client_secret",
    "token",
];

/// Remove credential-shaped text.
///
/// Idempotent: redacting already-redacted text changes nothing and reports
/// nothing, so a value cannot pick up a second reason code by passing through
/// twice. Every rule leaves its own placeholder alone. This was documented
/// long before it was true — only the provider-key rule held it, and the
/// header, assignment, and URL rules re-fired on `[redacted]` — which made
/// [`carries_credential`] answer yes for text that was already safe.
#[must_use]
pub fn redact(text: &str) -> Redacted {
    let mut codes = Vec::new();
    let mut result = text.to_owned();

    // Ordered from the most structural to the least, so a private key block is
    // removed whole rather than being partly eaten by a narrower rule first.
    result = remove_private_key_blocks(&result, &mut codes);
    result = remove_url_credentials(&result, &mut codes);
    result = remove_authorization_headers(&result, &mut codes);
    result = remove_credential_assignments(&result, &mut codes);
    result = remove_provider_keys(&result, &mut codes);

    codes.sort_unstable();
    codes.dedup();
    Redacted {
        text: result,
        reason_codes: codes,
    }
}

/// Whether `text` still contains anything that looks like a credential.
///
/// The assertion a test makes, and the check a boundary makes before it lets
/// something out. Separate from [`redact`] because "did this work?" and "make
/// this safe" are different questions, and a boundary that answered the first
/// by doing the second would silently paper over a leak upstream.
#[must_use]
pub fn carries_credential(text: &str) -> bool {
    redact(text).changed()
}

/// Characters that continue a key once its prefix has matched.
fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Replace `sk-…`, `ghp_…`, and their kin.
///
/// Every occurrence of every prefix is considered. The first version looked
/// only at the first place each prefix appeared, so an innocent match such as
/// the `sk-` in "task-runner" shielded every real key after it.
fn remove_provider_keys(text: &str, codes: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((start, end)) = next_provider_key(rest) {
        out.push_str(&rest[..start]);
        out.push_str(PLACEHOLDER);
        codes.push(reason::PROVIDER_API_KEY.to_owned());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The earliest provider key in `text`, as a byte range.
fn next_provider_key(text: &str) -> Option<(usize, usize)> {
    KEY_PREFIXES
        .iter()
        .filter_map(|prefix| {
            text.match_indices(prefix).find_map(|(start, _)| {
                let after = &text[start + prefix.len()..];
                let run = after.find(|c| !is_key_char(c)).unwrap_or(after.len());
                // A prefix that begins mid-identifier is part of something
                // else, not the start of a key.
                let mid_identifier = text[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|before| is_key_char(before) && before != '-');
                // Long enough that prose cannot reach it by accident. The
                // shortest real key of these shapes is well above this.
                (run >= 16 && !mid_identifier).then_some((start, start + prefix.len() + run))
            })
        })
        .min()
}

/// Replace the value of an `Authorization:` header.
fn remove_authorization_headers(text: &str, codes: &mut Vec<String>) -> String {
    replace_lines(text, codes, |line| {
        let lowered = line.to_ascii_lowercase();
        let marker = lowered.find("authorization:")?;
        let value_start = marker + "authorization:".len();
        let value = line[value_start..].trim();
        // Already this rule's own output. Re-reporting it would claim a second
        // credential was removed where there was none.
        if value.is_empty() || value == PLACEHOLDER {
            return None;
        }
        Some((
            format!("{} {PLACEHOLDER}", &line[..value_start]),
            reason::AUTHORIZATION_HEADER,
        ))
    })
}

/// Replace `NAME=value` where the name says the value is a credential.
fn remove_credential_assignments(text: &str, codes: &mut Vec<String>) -> String {
    replace_lines(text, codes, |line| {
        // Both shapes, so `API_KEY=x` and `"apiKey": "x"` are both caught.
        let separator = line.find('=').or_else(|| line.find(':'))?;
        let raw_name = line[..separator].trim().trim_matches(['"', '\'', ',', '{']);
        // A shell assignment keeps its keyword: `export DB_PASSWORD=x` names
        // `DB_PASSWORD`, not a name with a space in it.
        let raw_name = ["export ", "set "]
            .iter()
            .find_map(|keyword| raw_name.strip_prefix(keyword))
            .map_or(raw_name, str::trim_start);

        // A name with a space in it is prose, not an assignment. Without this
        // the sentence "the token: parameter is documented above" is read as a
        // credential named "the token" — a false positive found by the test
        // that lists innocent text, which is why that test lists it.
        if raw_name.chars().any(char::is_whitespace) {
            return None;
        }

        // `apiKey` normalizes to `apikey`; `OPENAI_API_KEY` ends with `api_key`.
        let name = raw_name.to_ascii_lowercase().replace('-', "_");
        if !CREDENTIAL_NAMES
            .iter()
            .any(|credential| name == *credential || name.ends_with(&format!("_{credential}")))
        {
            return None;
        }
        let value = line[separator + 1..].trim().trim_matches(['"', '\'', ',']);
        if value.is_empty() || value == PLACEHOLDER {
            return None;
        }
        // The separator is preserved, so a redacted JSON line still reads as
        // JSON and a redacted env line still reads as an env line.
        Some((
            format!(
                "{}{}{PLACEHOLDER}",
                &line[..separator],
                &line[separator..=separator]
            ),
            reason::CREDENTIAL_ASSIGNMENT,
        ))
    })
}

/// Replace the password in `scheme://user:password@host`, in every URL.
///
/// Two bugs lived in the first version. It searched for the next `@` anywhere
/// in the text, so an email address later in a sentence was read as the end of
/// an earlier URL's userinfo — and when that span held a `/`, it gave up and
/// appended the rest of the text unscanned. And after redacting one URL it
/// appended the remainder unscanned too. Either way, a second credential URL
/// in the same text kept its password.
///
/// Userinfo can only occur inside the authority, so the search is bounded by
/// it, and scanning resumes after every authority.
fn remove_url_credentials(text: &str, codes: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(scheme) = rest.find("://") {
        let authority_start = scheme + 3;
        out.push_str(&rest[..authority_start]);
        let after = &rest[authority_start..];
        // The authority ends at the first `/`, `?`, `#`, or whitespace.
        let authority_end = after
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .unwrap_or(after.len());
        let authority = &after[..authority_end];

        // The host follows the *last* `@`; a password is what follows the
        // first `:` of the userinfo before it.
        let credential = authority.rsplit_once('@').and_then(|(userinfo, host)| {
            userinfo
                .split_once(':')
                .map(|(user, password)| (user, password, host))
        });
        match credential {
            Some((user, password, host)) if !password.is_empty() && password != PLACEHOLDER => {
                out.push_str(user);
                out.push(':');
                out.push_str(PLACEHOLDER);
                out.push('@');
                out.push_str(host);
                codes.push(reason::URL_CREDENTIALS.to_owned());
            }
            _ => out.push_str(authority),
        }
        rest = &after[authority_end..];
    }
    out.push_str(rest);
    out
}

/// Replace a PEM private key block with a single placeholder line.
fn remove_private_key_blocks(text: &str, codes: &mut Vec<String>) -> String {
    if !text.contains("-----BEGIN") || !text.contains("PRIVATE KEY-----") {
        return text.to_owned();
    }
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY-----") {
            inside = true;
            out.push(PLACEHOLDER.to_owned());
            codes.push(reason::PRIVATE_KEY_BLOCK.to_owned());
            continue;
        }
        if inside {
            // The end marker is consumed with the block: leaving it behind
            // would be a record that says a key was here, which is the part
            // worth keeping, without the key.
            if line.contains("-----END") && line.contains("PRIVATE KEY-----") {
                inside = false;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    let joined = out.join("\n");
    if text.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

/// Rewrite whole lines that `rule` recognizes.
fn replace_lines(
    text: &str,
    codes: &mut Vec<String>,
    rule: impl Fn(&str) -> Option<(String, &'static str)>,
) -> String {
    let mut changed = false;
    let rewritten: Vec<String> = text
        .lines()
        .map(|line| match rule(line) {
            Some((replacement, code)) => {
                codes.push(code.to_owned());
                changed = true;
                replacement
            }
            None => line.to_owned(),
        })
        .collect();
    if !changed {
        return text.to_owned();
    }
    let joined = rewritten.join("\n");
    if text.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

/// The hash of `text` after redaction.
///
/// What a context source records. Hashing the raw text would make the record
/// name content that was never released, and would make the hash itself a
/// (weak) oracle for a secret whose format is known.
#[must_use]
pub fn redacted_hash(text: &str) -> String {
    content_hash(&redact(text).text)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    /// Every value here is synthetic (`TESTING_STRATEGY.md` §5).
    const OPENAI_KEY: &str = "sk-proj-A1b2C3d4E5f6G7h8I9j0K1l2M3n4";
    const GITHUB_TOKEN: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";

    #[test]
    fn a_pasted_provider_key_does_not_survive() {
        let result = redact(&format!("use {OPENAI_KEY} for the call"));
        assert_eq!(result.text, format!("use {PLACEHOLDER} for the call"));
        assert_eq!(result.reason_codes, vec![reason::PROVIDER_API_KEY]);
    }

    #[test]
    fn several_keys_of_different_shapes_are_all_removed() {
        let result = redact(&format!("{OPENAI_KEY} and {GITHUB_TOKEN}"));
        assert!(!result.text.contains("sk-proj"), "{}", result.text);
        assert!(!result.text.contains("ghp_"), "{}", result.text);
        // One code, not two: the record says a provider key was removed, and
        // saying it twice tells the reader nothing more.
        assert_eq!(result.reason_codes, vec![reason::PROVIDER_API_KEY]);
    }

    #[test]
    fn ordinary_prose_and_identifiers_are_left_alone() {
        // The false-positive cases that make a redactor worse than nothing.
        for innocent in [
            "the sk- prefix identifies an OpenAI key",
            "commit 4f8c2a91b3d7e6054a1c9f2e8b7d3a6c5e4f1029",
            "550e8400-e29b-41d4-a716-446655440000",
            "https://example.com/path?query=value",
            "Ask me about my_secret_plan for the weekend",
            "the token: parameter is documented above",
        ] {
            let result = redact(innocent);
            assert_eq!(result.text, innocent, "redacted innocent text: {innocent}");
            assert!(!result.changed(), "{innocent}");
        }
    }

    #[test]
    fn an_authorization_header_keeps_its_name_and_loses_its_value() {
        let result = redact("Authorization: Bearer abc123def456\nAccept: text/event-stream");
        assert_eq!(
            result.text,
            "Authorization: [redacted]\nAccept: text/event-stream"
        );
        assert_eq!(result.reason_codes, vec![reason::AUTHORIZATION_HEADER]);
    }

    #[test]
    fn a_credential_assignment_loses_its_value_and_keeps_its_name() {
        // The name is the useful half: "your OPENAI_API_KEY was removed" is
        // actionable, and the value never needed to be in the record.
        for (line, expected) in [
            ("OPENAI_API_KEY=sk-notreal", "OPENAI_API_KEY=[redacted]"),
            ("password = hunter2", "password =[redacted]"),
            ("  \"apiKey\": \"abcdef\"", "  \"apiKey\":[redacted]"),
            ("client_secret: abc123", "client_secret:[redacted]"),
        ] {
            let result = redact(line);
            assert_eq!(result.text, expected, "for {line}");
            assert_eq!(result.reason_codes, vec![reason::CREDENTIAL_ASSIGNMENT]);
        }
    }

    #[test]
    fn an_innocent_prefix_match_does_not_shield_a_later_key() {
        // `sk-` inside "task-runner" was rejected as mid-identifier, and the
        // scan never looked past it, so the real key went through untouched.
        for text in [
            format!("the task-runner key is {OPENAI_KEY}"),
            format!("disk-0123456789abcdef0 then {OPENAI_KEY}"),
            format!("sk-short then {OPENAI_KEY}"),
            format!("Xghp_0123456789abcdefghij then {GITHUB_TOKEN}"),
        ] {
            let result = redact(&text);
            assert!(!result.text.contains("A1b2C3d4"), "{}", result.text);
            assert!(
                !result.text.contains("ghp_0123456789abcdefghijklmn"),
                "{}",
                result.text
            );
            assert_eq!(result.reason_codes, vec![reason::PROVIDER_API_KEY]);
        }
        assert_eq!(
            redact(&format!("the task-runner key is {OPENAI_KEY}")).text,
            format!("the task-runner key is {PLACEHOLDER}")
        );
    }

    #[test]
    fn a_shell_assignment_loses_its_value() {
        for (line, expected) in [
            (
                "export DB_PASSWORD=hunter2hunter2",
                "export DB_PASSWORD=[redacted]",
            ),
            ("set API_TOKEN=abc123", "set API_TOKEN=[redacted]"),
        ] {
            let result = redact(line);
            assert_eq!(result.text, expected, "for {line}");
            assert_eq!(result.reason_codes, vec![reason::CREDENTIAL_ASSIGNMENT]);
            assert!(!redact(&result.text).changed(), "{line}");
        }
    }

    #[test]
    fn a_url_password_goes_and_the_rest_of_the_url_stays() {
        // The host is diagnostic information worth keeping; the password is
        // not, and the username is how a person recognizes which one it was.
        let result = redact("clone https://gasper:hunter2@git.example.com/repo.git now");
        assert_eq!(
            result.text,
            "clone https://gasper:[redacted]@git.example.com/repo.git now"
        );
        assert_eq!(result.reason_codes, vec![reason::URL_CREDENTIALS]);
    }

    #[test]
    fn a_url_without_a_password_is_untouched() {
        let untouched = "see https://user@example.com/page for details";
        assert_eq!(redact(untouched).text, untouched);
    }

    #[test]
    fn a_private_key_block_is_removed_whole() {
        let result = redact(
            "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nIBAAKC\n-----END RSA PRIVATE KEY-----\nafter",
        );
        assert_eq!(result.text, "before\n[redacted]\nafter");
        assert!(!result.text.contains("MIIEow"));
        assert_eq!(result.reason_codes, vec![reason::PRIVATE_KEY_BLOCK]);
    }

    #[test]
    fn every_credential_url_in_a_text_is_redacted_not_just_the_first() {
        // After the first credential URL, the rest of the text used to be
        // appended unscanned, so a second URL's password went straight
        // through.
        let result = redact("mirror https://a:first-pw@one.test and https://b:second-pw@two.test");
        assert!(!result.text.contains("first-pw"), "{}", result.text);
        assert!(!result.text.contains("second-pw"), "{}", result.text);
    }

    #[test]
    fn a_distant_at_sign_does_not_end_the_scan() {
        // The scan looked for the next `@` anywhere in the text, so an email
        // address later in the sentence was read as the end of an earlier
        // URL's userinfo — which then stopped the scan before a real
        // credential further on.
        // The port's colon is what reaches the leaking branch: without a colon
        // before the distant `@`, the old code skipped past it harmlessly — and
        // the first version of this test passed against the bug.
        let text = "docs at https://example.test:8443/guide, mail me@example.test, clone https://u:leaked-pw@git.test/r";
        let result = redact(text);
        assert!(!result.text.contains("leaked-pw"), "{}", result.text);
        assert!(
            result.text.contains("https://example.test:8443/guide"),
            "{}",
            result.text
        );
        assert!(result.text.contains("me@example.test"), "{}", result.text);
    }

    #[test]
    fn every_rule_leaves_its_own_placeholder_alone() {
        // The earlier idempotence test covered one rule. The others re-fired on
        // their own output: `NAME=[redacted]` still has a credential name and a
        // non-empty value, so a second pass reported a code for nothing.
        for input in [
            "Authorization: Bearer abc123def456",
            "OPENAI_API_KEY=sk-notreal",
            "password = hunter2",
            "  \"apiKey\": \"abcdef\"",
            "client_secret: abc123",
            "clone https://gasper:hunter2@git.example.com/repo.git now",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----",
            OPENAI_KEY,
        ] {
            let once = redact(input);
            assert!(once.changed(), "nothing was redacted in {input:?}");
            let twice = redact(&once.text);
            assert_eq!(twice.text, once.text, "second pass rewrote {input:?}");
            assert!(
                !twice.changed(),
                "second pass over {input:?} reported {:?}",
                twice.reason_codes
            );
        }
    }

    #[test]
    fn already_redacted_text_is_not_mistaken_for_a_credential() {
        // `carries_credential` is the check a boundary makes. Answering yes for
        // text that is already safe would refuse work that should be allowed.
        for redacted in [
            "OPENAI_API_KEY=[redacted]",
            "Authorization: [redacted]",
            "https://gasper:[redacted]@git.example.com/repo.git",
        ] {
            assert!(!carries_credential(redacted), "{redacted}");
        }
    }

    #[test]
    fn redacting_twice_changes_nothing_the_second_time() {
        // Otherwise a value passing two boundaries collects a reason code at
        // each, and the record implies two different things were removed.
        let once = redact(&format!("key {OPENAI_KEY}"));
        let twice = redact(&once.text);
        assert_eq!(twice.text, once.text);
        assert!(!twice.changed(), "{:?}", twice.reason_codes);
    }

    #[test]
    fn the_placeholder_does_not_leak_the_length_of_what_it_replaced() {
        let short = redact("sk-aaaaaaaaaaaaaaaaaaaa");
        let long = redact(&format!("sk-{}", "a".repeat(400)));
        assert_eq!(short.text, long.text);
    }

    #[test]
    fn a_hash_names_the_redacted_text_not_the_original() {
        // A context source that hashed the raw text would name content that
        // was never released.
        let raw = format!("call with {OPENAI_KEY}");
        assert_eq!(redacted_hash(&raw), content_hash(&redact(&raw).text));
        assert_ne!(redacted_hash(&raw), content_hash(&raw));
    }

    #[test]
    fn carrying_a_credential_is_answerable_without_redacting() {
        assert!(carries_credential(&format!("here: {GITHUB_TOKEN}")));
        assert!(!carries_credential("here: nothing sensitive"));
    }
}
