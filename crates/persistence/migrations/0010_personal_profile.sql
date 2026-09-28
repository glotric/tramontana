-- The person's profile and memory notes (ADR 0061).
--
-- One profile per database: the local profile is one person's. Kept apart
-- from every project, so no project bundle or repository ever carries it.
-- Fields are stored as the person saved them, already redacted by core.
CREATE TABLE personal_profile (
    profile_id      INTEGER PRIMARY KEY CHECK (profile_id = 1),
    name            TEXT,
    about           TEXT,
    answer_style    TEXT NOT NULL DEFAULT 'balanced',
    memory_enabled  INTEGER NOT NULL DEFAULT 1,
    schema_version  INTEGER NOT NULL
) STRICT;

CREATE TABLE memory_note (
    note_id     TEXT PRIMARY KEY,
    text        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    position    INTEGER NOT NULL
) STRICT;
