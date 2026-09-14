-- Record the commands a project declares in `.tramontana/project.md`.
--
-- A task resumed after a restart reads its configuration back from here, and
-- runs a recipe only as this record describes it (ADR 0025). Without the
-- column, recipes would silently vanish on reload and a resumed task would
-- find none.
--
-- Additive with a default, so an existing profile migrates without rewriting
-- rows. A manifest resolved before this migration declared no recipes this
-- build could read, and reads back as declaring none.
ALTER TABLE configuration_source
    ADD COLUMN recipes TEXT NOT NULL DEFAULT '[]';
