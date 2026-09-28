-- Regular work Tramontana does while it is open (ADR 0063).
--
-- The cadence and local wall-clock times are JSON: they are kept and compared
-- as the person means them ("weekdays at 8:00"), not as instants. A schedule's
-- conversation is not a foreign key: deleting the conversation leaves the
-- schedule to start a new one, and deleting the schedule keeps its history.
CREATE TABLE schedule (
    schedule_id      TEXT PRIMARY KEY,
    title            TEXT NOT NULL,
    goal             TEXT NOT NULL,
    project_id       TEXT REFERENCES project(project_id) ON DELETE CASCADE,
    conversation_id  TEXT NOT NULL,
    cadence          TEXT NOT NULL,
    routing_mode     TEXT NOT NULL,
    enabled          INTEGER NOT NULL,
    created_at       TEXT NOT NULL,
    created_local    TEXT NOT NULL,
    last_run         TEXT,
    last_task_id     TEXT,
    schema_version   INTEGER NOT NULL
) STRICT;
