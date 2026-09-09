-- Record what was redacted out of an instruction file at resolution.
--
-- `content_hash` and `byte_count` in this table now describe the *redacted*
-- text rather than the bytes on disk, because a hash naming content that may
-- not be released would let a later prompt assembly send the original and
-- still satisfy every record describing it (`SECURITY_AND_PERMISSIONS.md` §7).
-- This column says which rules fired, so the change is visible rather than
-- inferred from a hash that quietly differs.
--
-- Additive with a default, so an existing profile migrates without rewriting
-- rows. A manifest resolved before this migration has no redaction codes and
-- reads back as having none, which is accurate: at the time it was written,
-- nothing was redacted. Its hashes still name what it hashed. Re-resolving the
-- project produces a new manifest hash for any file that did contain a
-- credential, which is the correct outcome — it is a different release.
ALTER TABLE configuration_source
    ADD COLUMN redactions TEXT NOT NULL DEFAULT '[]';
