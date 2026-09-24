-- Keep the keywords a skill declares as hints that it may be relevant.
--
-- `CONFIGURATION.md` §5 gives a skill `suggest_when`, and ADR 0054 uses it to
-- say which of a project's skills look relevant to a goal. Both applications
-- read the configuration record for that, so without the column the keywords
-- are parsed and thrown away and nothing can be suggested.
--
-- Additive with a default, so an existing profile migrates without rewriting
-- rows. A manifest resolved before this migration declared no keywords this
-- build could read, and reads back declaring none.
ALTER TABLE configuration_source
    ADD COLUMN suggest_when TEXT NOT NULL DEFAULT '[]';
