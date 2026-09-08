-- Initial schema for the local operational plane.
--
-- `PROJECT_FORMAT.md` §2.2: this database holds project identity and approved
-- roots, conversation and task records, the normalized event stream, context
-- and routing decisions, and secret *references*. It never holds secret values.
--
-- A shipped migration is immutable (`DEVELOPMENT.md` §7). Correct it with a
-- later migration, never by editing this file.

CREATE TABLE project (
    project_id   TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    created_at   TEXT NOT NULL
) STRICT;

-- Approved roots are per-project and canonical. Storing the canonical form is
-- what makes later containment checks meaningful: a grant recorded as a
-- symlink would authorize wherever the link happened to point that day.
CREATE TABLE project_root (
    project_id     TEXT NOT NULL REFERENCES project(project_id) ON DELETE CASCADE,
    canonical_path TEXT NOT NULL,
    granted_at     TEXT NOT NULL,
    PRIMARY KEY (project_id, canonical_path)
) STRICT;

CREATE TABLE conversation (
    conversation_id TEXT PRIMARY KEY,
    project_id      TEXT REFERENCES project(project_id) ON DELETE CASCADE,
    title           TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    archived_at     TEXT
) STRICT;

CREATE INDEX conversation_by_project ON conversation(project_id);

CREATE TABLE task (
    task_id         TEXT PRIMARY KEY,
    conversation_id TEXT REFERENCES conversation(conversation_id) ON DELETE CASCADE,
    project_id      TEXT REFERENCES project(project_id) ON DELETE CASCADE,
    goal_summary    TEXT NOT NULL,
    state           TEXT NOT NULL,
    last_sequence   INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;

CREATE INDEX task_by_conversation ON task(conversation_id);

-- The durable event stream. `(task_id, sequence)` is the primary key rather
-- than an index, so the database itself refuses two events at the same
-- position: the gapless-sequence invariant survives a bug in the writer.
CREATE TABLE task_event (
    task_id         TEXT NOT NULL,
    sequence        INTEGER NOT NULL,
    event_id        TEXT NOT NULL UNIQUE,
    conversation_id TEXT,
    occurred_at     TEXT NOT NULL,
    kind            TEXT NOT NULL,
    visibility      TEXT NOT NULL,
    correlation_id  TEXT NOT NULL,
    causation_id    TEXT,
    schema_version  INTEGER NOT NULL,
    payload         TEXT NOT NULL,
    PRIMARY KEY (task_id, sequence)
) STRICT;

-- Where an instruction file was loaded from, what it hashed to, and whether it
-- validated. Content is not stored: the file is the source of truth, and a
-- hash is enough to explain later behavior without duplicating project data.
CREATE TABLE configuration_source (
    source_id     TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES project(project_id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    kind          TEXT NOT NULL,
    content_hash  TEXT NOT NULL,
    precedence    INTEGER NOT NULL,
    enabled       INTEGER NOT NULL,
    loaded_at     TEXT NOT NULL
) STRICT;

CREATE INDEX configuration_source_by_project ON configuration_source(project_id);
