use std::collections::BTreeMap;

/// The documented front-matter subset (`CONFIGURATION.md` §5).
///
/// Only these keys carry meaning. Anything else is preserved in
/// [`FrontMatter::unknown_keys`] and reported as a warning, so a file authored
/// for a newer version survives a round trip through an older one rather than
/// being silently rewritten.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontMatter {
    /// Stable identifier, unique within its kind.
    pub id: Option<String>,
    /// Display name.
    pub name: Option<String>,
    /// One-line description.
    pub description: Option<String>,
    /// Author-assigned version.
    pub version: Option<i64>,
    /// Whether the item is active. Absent means enabled.
    pub enabled: Option<bool>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Priority for rules; higher applies first within the native rule layer.
    pub priority: Option<i64>,
    /// Portable glob scopes for native rules and skills.
    pub applies_to: Vec<String>,
    /// Capabilities the definition says it needs. Informational only.
    pub required_capabilities: Vec<String>,
    /// Capabilities the definition would prefer. Informational only.
    pub preferred_capabilities: Vec<String>,
    /// Keywords which may suggest a skill. Informational only.
    pub suggest_when: Vec<String>,
    /// Maximum workflow/agent step hint.
    pub max_steps: Option<i64>,
    /// Portable project format version.
    pub format_version: Option<i64>,
    /// Keys this build does not recognize, preserved for portability.
    pub unknown_keys: Vec<String>,
}

/// Why front matter could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FrontMatterError {
    /// The opening `---` was never closed.
    #[error("front matter is not terminated by a closing `---`")]
    Unterminated,
    /// The block is not valid YAML.
    #[error("front matter is not valid YAML: {0}")]
    Malformed(String),
    /// The block is not a mapping.
    #[error("front matter must be a mapping of keys to values")]
    NotAMapping,
    /// A documented key has the wrong type.
    #[error("`{key}` must be {expected}")]
    WrongType {
        /// The offending key.
        key: &'static str,
        /// What was expected.
        expected: &'static str,
    },
    /// Mapping keys must be strings so none are silently discarded.
    #[error("front-matter keys must be strings")]
    NonStringKey,
    /// An identifier does not satisfy the portable identifier grammar.
    #[error("`id` must match [a-z0-9][a-z0-9._-]*")]
    InvalidId,
    /// The document exceeds a safety limit.
    #[error("front matter exceeds the {limit} limit")]
    TooLarge {
        /// Which limit was hit.
        limit: &'static str,
    },
}

/// Largest front-matter block accepted, in bytes.
///
/// Instruction files are untrusted input like any other
/// (`SECURITY_AND_PERMISSIONS.md` §2). A bound is cheap here and expensive to
/// retrofit after something has already allocated.
const MAX_FRONT_MATTER_BYTES: usize = 64 * 1024;

/// Largest number of top-level keys accepted.
const MAX_KEYS: usize = 128;

/// Largest number of tags accepted.
const MAX_TAGS: usize = 64;

/// Largest nesting depth accepted from YAML input.
const MAX_DEPTH: usize = 16;

/// Split a document into its front matter and its Markdown body.
///
/// A document with no leading `---` has no front matter, which is normal:
/// `AGENTS.md` and `CLAUDE.md` are ordinary Markdown (`CONFIGURATION.md` §10).
///
/// # Errors
///
/// [`FrontMatterError`] when a block is opened but cannot be read. The caller
/// disables the item and records a diagnostic rather than failing.
pub fn split_front_matter(document: &str) -> Result<(FrontMatter, &str), FrontMatterError> {
    let Some(rest) = document
        .strip_prefix("---\n")
        .or_else(|| document.strip_prefix("---\r\n"))
    else {
        return Ok((FrontMatter::default(), document));
    };

    let Some((end, fence_len)) = find_closing_fence(rest) else {
        return Err(FrontMatterError::Unterminated);
    };
    let (block, remainder) = rest.split_at(end);

    if block.len() > MAX_FRONT_MATTER_BYTES {
        return Err(FrontMatterError::TooLarge { limit: "size" });
    }

    let body = &remainder[fence_len..];

    Ok((parse(block)?, body))
}

/// Find the offset of the closing `---` line.
fn find_closing_fence(rest: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line == "---\n" || line == "---\r\n" || line == "---" {
            return Some((offset, line.len()));
        }
        offset += line.len();
    }
    None
}

/// Parse the documented subset out of a YAML mapping.
fn parse(block: &str) -> Result<FrontMatter, FrontMatterError> {
    use yaml_rust2::{Yaml, YamlLoader};

    let documents = YamlLoader::load_from_str(block)
        .map_err(|error| FrontMatterError::Malformed(error.to_string()))?;

    // An empty block is valid and means "no metadata".
    let Some(document) = documents.first() else {
        return Ok(FrontMatter::default());
    };
    let Yaml::Hash(mapping) = document else {
        return Err(FrontMatterError::NotAMapping);
    };
    if yaml_depth(document) > MAX_DEPTH {
        return Err(FrontMatterError::TooLarge { limit: "depth" });
    }
    if mapping.len() > MAX_KEYS {
        return Err(FrontMatterError::TooLarge { limit: "key count" });
    }

    let mut fields: BTreeMap<String, &Yaml> = BTreeMap::new();
    for (key, value) in mapping {
        let Yaml::String(name) = key else {
            return Err(FrontMatterError::NonStringKey);
        };
        fields.insert(name.clone(), value);
    }

    let mut parsed = FrontMatter::default();

    for (key, value) in &fields {
        match key.as_str() {
            "id" => {
                let id = string(value, "id")?;
                if !valid_id(&id) {
                    return Err(FrontMatterError::InvalidId);
                }
                parsed.id = Some(id);
            }
            "name" => parsed.name = Some(string(value, "name")?),
            "description" => parsed.description = Some(string(value, "description")?),
            "version" => parsed.version = Some(integer(value, "version")?),
            "priority" => parsed.priority = Some(integer(value, "priority")?),
            "enabled" => {
                parsed.enabled = Some(value.as_bool().ok_or(FrontMatterError::WrongType {
                    key: "enabled",
                    expected: "a boolean",
                })?);
            }
            "tags" => {
                let Yaml::Array(items) = value else {
                    return Err(FrontMatterError::WrongType {
                        key: "tags",
                        expected: "a list of strings",
                    });
                };
                if items.len() > MAX_TAGS {
                    return Err(FrontMatterError::TooLarge { limit: "tag count" });
                }
                parsed.tags = string_list(items, "tags")?;
            }
            // Documented per-kind keys that this milestone records but does not
            // yet act on. Listing them keeps them out of `unknown_keys`, so a
            // valid example file does not produce a spurious warning.
            "applies_to" => {
                parsed.applies_to = string_array(value, "applies_to")?;
            }
            "required_capabilities" => {
                parsed.required_capabilities = string_array(value, "required_capabilities")?;
            }
            "preferred_capabilities" => {
                parsed.preferred_capabilities = string_array(value, "preferred_capabilities")?;
            }
            "suggest_when" => {
                parsed.suggest_when = string_array(value, "suggest_when")?;
            }
            "max_steps" => {
                let steps = integer(value, "max_steps")?;
                if steps < 1 {
                    return Err(FrontMatterError::WrongType {
                        key: "max_steps",
                        expected: "a positive integer",
                    });
                }
                parsed.max_steps = Some(steps);
            }
            "format_version" => {
                parsed.format_version = Some(integer(value, "format_version")?);
            }
            "inputs" => {
                let yaml_rust2::Yaml::Array(items) = value else {
                    return Err(FrontMatterError::WrongType {
                        key: "inputs",
                        expected: "a list",
                    });
                };
                if items.len() > MAX_TAGS {
                    return Err(FrontMatterError::TooLarge {
                        limit: "list length",
                    });
                }
            }
            other => parsed.unknown_keys.push(other.to_owned()),
        }
    }

    Ok(parsed)
}

fn string_array(
    value: &yaml_rust2::Yaml,
    key: &'static str,
) -> Result<Vec<String>, FrontMatterError> {
    let yaml_rust2::Yaml::Array(items) = value else {
        return Err(FrontMatterError::WrongType {
            key,
            expected: "a list of strings",
        });
    };
    if items.len() > MAX_TAGS {
        return Err(FrontMatterError::TooLarge {
            limit: "list length",
        });
    }
    string_list(items, key)
}

fn string_list(
    items: &[yaml_rust2::Yaml],
    key: &'static str,
) -> Result<Vec<String>, FrontMatterError> {
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or(FrontMatterError::WrongType {
                    key,
                    expected: "a list of strings",
                })
        })
        .collect()
}

fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn yaml_depth(value: &yaml_rust2::Yaml) -> usize {
    use yaml_rust2::Yaml;
    match value {
        Yaml::Array(items) => 1 + items.iter().map(yaml_depth).max().unwrap_or(0),
        Yaml::Hash(items) => {
            1 + items
                .iter()
                .map(|(key, value)| yaml_depth(key).max(yaml_depth(value)))
                .max()
                .unwrap_or(0)
        }
        _ => 1,
    }
}

fn string(value: &yaml_rust2::Yaml, key: &'static str) -> Result<String, FrontMatterError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or(FrontMatterError::WrongType {
            key,
            expected: "a string",
        })
}

fn integer(value: &yaml_rust2::Yaml, key: &'static str) -> Result<i64, FrontMatterError> {
    value.as_i64().ok_or(FrontMatterError::WrongType {
        key,
        expected: "an integer",
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_document_without_front_matter_is_all_body() {
        // AGENTS.md and CLAUDE.md are ordinary Markdown and must load as-is.
        let (matter, body) = split_front_matter("# Instructions\n\nDo the thing.\n").unwrap();
        assert_eq!(matter, FrontMatter::default());
        assert_eq!(body, "# Instructions\n\nDo the thing.\n");
    }

    #[test]
    fn the_documented_subset_is_parsed() {
        let (matter, body) = split_front_matter(
            "---\nid: rust-quality\nname: Rust quality pass\ndescription: Format and lint.\n\
             version: 2\nenabled: false\ntags: [rust, review]\npriority: 100\n---\n# Purpose\n",
        )
        .unwrap();

        assert_eq!(matter.id.as_deref(), Some("rust-quality"));
        assert_eq!(matter.name.as_deref(), Some("Rust quality pass"));
        assert_eq!(matter.version, Some(2));
        assert_eq!(matter.enabled, Some(false));
        assert_eq!(matter.tags, vec!["rust", "review"]);
        assert_eq!(matter.priority, Some(100));
        assert!(matter.unknown_keys.is_empty());
        assert_eq!(body, "# Purpose\n");
    }

    #[test]
    fn unknown_keys_are_preserved_rather_than_dropped() {
        // A file authored for a newer version must survive a round trip
        // through an older one (`CONFIGURATION.md` §5).
        let (matter, _) =
            split_front_matter("---\nid: x\nfuture_field: 1\nanother: two\n---\nbody\n").unwrap();
        assert_eq!(matter.id.as_deref(), Some("x"));
        assert_eq!(matter.unknown_keys, vec!["another", "future_field"]);
    }

    #[test]
    fn documented_per_kind_keys_are_not_reported_as_unknown() {
        // The repository's own example files use these; warning about them
        // would train the reader to ignore warnings.
        let (matter, _) = split_front_matter(
            "---\nid: x\napplies_to: [\"**/*.rs\"]\nrequired_capabilities: [git.read]\n\
             max_steps: 40\n---\nbody\n",
        )
        .unwrap();
        assert!(matter.unknown_keys.is_empty(), "{:?}", matter.unknown_keys);
    }

    #[test]
    fn an_unterminated_block_is_an_error_not_a_body() {
        // Treating it as body would silently feed YAML to the model as if it
        // were instructions.
        let error = split_front_matter("---\nid: x\nstill going\n").unwrap_err();
        assert_eq!(error, FrontMatterError::Unterminated);
    }

    #[test]
    fn a_wrongly_typed_field_names_the_offending_key() {
        let error = split_front_matter("---\nenabled: \"yes\"\n---\nbody\n").unwrap_err();
        assert_eq!(
            error,
            FrontMatterError::WrongType {
                key: "enabled",
                expected: "a boolean"
            }
        );

        let error = split_front_matter("---\nversion: two\n---\nbody\n").unwrap_err();
        assert_eq!(
            error,
            FrontMatterError::WrongType {
                key: "version",
                expected: "an integer"
            }
        );

        let error = split_front_matter("---\ntags: [rust, 42]\n---\nbody\n").unwrap_err();
        assert_eq!(
            error,
            FrontMatterError::WrongType {
                key: "tags",
                expected: "a list of strings"
            }
        );

        let error =
            split_front_matter("---\nrequired_capabilities: [filesystem.read, false]\n---\nbody\n")
                .unwrap_err();
        assert_eq!(
            error,
            FrontMatterError::WrongType {
                key: "required_capabilities",
                expected: "a list of strings"
            }
        );
    }

    #[test]
    fn fences_are_exact_and_crlf_is_supported() {
        let (matter, body) = split_front_matter("---\r\nid: portable\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(matter.id.as_deref(), Some("portable"));
        assert_eq!(body, "body\r\n");

        let document = "----\nid: prose\n----\n";
        let (matter, body) = split_front_matter(document).unwrap();
        assert_eq!(matter, FrontMatter::default());
        assert_eq!(body, document);
    }

    #[test]
    fn identifiers_use_the_documented_portable_grammar() {
        for invalid in ["", "Uppercase", "-leading", "contains space", "naïve"] {
            let yaml_id = if invalid.is_empty() {
                "\"\"".to_owned()
            } else {
                invalid.to_owned()
            };
            assert_eq!(
                split_front_matter(&format!("---\nid: {yaml_id}\n---\n")).unwrap_err(),
                FrontMatterError::InvalidId,
                "accepted {invalid:?}"
            );
        }
        split_front_matter("---\nid: rust.review_2\n---\n").unwrap();
    }

    #[test]
    fn non_string_mapping_keys_are_rejected() {
        let error = split_front_matter("---\n1: value\n---\n").unwrap_err();
        assert_eq!(error, FrontMatterError::NonStringKey);
    }

    #[test]
    fn a_scalar_document_is_not_a_mapping() {
        let error = split_front_matter("---\njust a string\n---\nbody\n").unwrap_err();
        assert_eq!(error, FrontMatterError::NotAMapping);
    }

    #[test]
    fn an_empty_block_is_valid_and_means_no_metadata() {
        let (matter, body) = split_front_matter("---\n---\nbody\n").unwrap();
        assert_eq!(matter, FrontMatter::default());
        assert_eq!(body, "body\n");
    }

    #[test]
    fn an_oversized_block_is_refused() {
        // Untrusted input gets a bound. Without one, a hostile file in a
        // repository someone cloned decides how much memory to allocate.
        let huge = format!("---\nid: x\n{}\n---\nbody\n", "# ".repeat(40_000));
        let error = split_front_matter(&huge).unwrap_err();
        assert_eq!(error, FrontMatterError::TooLarge { limit: "size" });
    }

    #[test]
    fn too_many_tags_are_refused() {
        let tags: Vec<String> = (0..100).map(|n| format!("t{n}")).collect();
        let document = format!("---\ntags: [{}]\n---\nbody\n", tags.join(", "));
        let error = split_front_matter(&document).unwrap_err();
        assert_eq!(error, FrontMatterError::TooLarge { limit: "tag count" });
    }

    #[test]
    fn front_matter_cannot_grant_anything() {
        // The parser has no field for a root, a capability grant, a secret, or
        // a routing destination, so a file cannot express one. This test exists
        // to fail loudly if someone adds such a field later.
        let (matter, _) = split_front_matter(
            "---\nid: hostile\nroots: [\"/\"]\ngrant: filesystem.write\n\
             secret: sk-live-123\nrouting: cloudPreferred\n---\nbody\n",
        )
        .unwrap();
        assert_eq!(matter.id.as_deref(), Some("hostile"));
        assert_eq!(
            matter.unknown_keys,
            vec!["grant", "roots", "routing", "secret"],
            "these must remain unrecognized, not become behavior"
        );
    }
}
