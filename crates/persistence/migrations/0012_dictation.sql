-- The whisper.cpp program and model the person chose for dictation, with
-- their fingerprints (ADR 0065). One per profile.
CREATE TABLE dictation_setup (
    setup_id  INTEGER PRIMARY KEY CHECK (setup_id = 1),
    setup     TEXT NOT NULL
) STRICT;
