-- Keep the one-line description a skill, agent profile or workflow declares.
--
-- A person choosing which skills a task uses needs to know what each one does,
-- and the configuration record is where both applications read that from
-- (ADR 0053). Without the column the description is parsed and thrown away,
-- and the chooser can offer only file names.
--
-- Additive and nullable, so an existing profile migrates without rewriting
-- rows. A manifest resolved before this migration reads back with no
-- description, and gains one the next time its project is resolved.
ALTER TABLE configuration_source
    ADD COLUMN declared_description TEXT;
