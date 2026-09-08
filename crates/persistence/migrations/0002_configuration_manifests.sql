-- Expand M2's initial persistence spike into the normalized operational plane.
-- Migration 0001 is retained byte-for-byte so development profiles created by
-- the spike remain recoverable.

-- The 0001 table used source_id as a global primary key and did not associate
-- sources with a particular resolution. No application path wrote rows before
-- this migration, so replacing the empty spike table loses no user data.
DROP INDEX configuration_source_by_project;
DROP TABLE configuration_source;

CREATE TABLE configuration_manifest (
    project_id     TEXT NOT NULL REFERENCES project(project_id) ON DELETE CASCADE,
    manifest_hash  TEXT NOT NULL,
    resolved_at    TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (project_id, manifest_hash)
) STRICT;

CREATE INDEX configuration_manifest_latest
    ON configuration_manifest(project_id, resolved_at DESC);

CREATE TABLE configuration_source (
    project_id             TEXT NOT NULL,
    manifest_hash          TEXT NOT NULL,
    position               INTEGER NOT NULL,
    source_id              TEXT NOT NULL,
    relative_path          TEXT NOT NULL,
    kind                   TEXT NOT NULL,
    declared_id            TEXT,
    declared_name          TEXT,
    scopes                 TEXT NOT NULL,
    requested_capabilities TEXT NOT NULL,
    priority               INTEGER,
    selection_order        INTEGER,
    enabled                INTEGER NOT NULL,
    active                 INTEGER NOT NULL,
    precedence             INTEGER NOT NULL,
    content_hash           TEXT NOT NULL,
    byte_count             INTEGER NOT NULL,
    unknown_keys           TEXT NOT NULL,
    PRIMARY KEY (project_id, manifest_hash, position),
    UNIQUE (project_id, manifest_hash, source_id),
    FOREIGN KEY (project_id, manifest_hash)
        REFERENCES configuration_manifest(project_id, manifest_hash) ON DELETE CASCADE
) STRICT;

CREATE TABLE configuration_diagnostic (
    project_id    TEXT NOT NULL,
    manifest_hash TEXT NOT NULL,
    position      INTEGER NOT NULL,
    severity      TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    message       TEXT NOT NULL,
    PRIMARY KEY (project_id, manifest_hash, position),
    FOREIGN KEY (project_id, manifest_hash)
        REFERENCES configuration_manifest(project_id, manifest_hash) ON DELETE CASCADE
) STRICT;

ALTER TABLE task RENAME TO task_v1;
DROP INDEX task_by_conversation;

CREATE TABLE task (
    task_id                    TEXT PRIMARY KEY,
    conversation_id            TEXT REFERENCES conversation(conversation_id) ON DELETE CASCADE,
    project_id                 TEXT REFERENCES project(project_id) ON DELETE CASCADE,
    goal_summary               TEXT NOT NULL,
    configuration_manifest_hash TEXT,
    state                      TEXT NOT NULL,
    last_sequence              INTEGER NOT NULL DEFAULT 0,
    created_at                 TEXT NOT NULL,
    updated_at                 TEXT NOT NULL,
    FOREIGN KEY (project_id, configuration_manifest_hash)
        REFERENCES configuration_manifest(project_id, manifest_hash)
) STRICT;

INSERT INTO task (
    task_id, conversation_id, project_id, goal_summary, state,
    last_sequence, created_at, updated_at
)
SELECT
    task_id, conversation_id, project_id, goal_summary, state,
    last_sequence, created_at, updated_at
FROM task_v1;

DROP TABLE task_v1;
CREATE INDEX task_by_conversation ON task(conversation_id);

-- Canonical normalized messages and checkpoints have their own durable rows.
-- Event payloads may reference them; provider-private transcripts never do.
CREATE TABLE message (
    message_id      TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversation(conversation_id) ON DELETE CASCADE,
    task_id         TEXT REFERENCES task(task_id) ON DELETE SET NULL,
    role            TEXT NOT NULL,
    status          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    parts           TEXT NOT NULL,
    provenance      TEXT NOT NULL
) STRICT;

CREATE INDEX message_by_conversation ON message(conversation_id, created_at, message_id);

CREATE TABLE task_checkpoint (
    checkpoint_id               TEXT PRIMARY KEY,
    task_id                     TEXT NOT NULL REFERENCES task(task_id) ON DELETE CASCADE,
    event_sequence              INTEGER NOT NULL,
    created_at                  TEXT NOT NULL,
    task_state                  TEXT NOT NULL,
    configuration_manifest_hash TEXT,
    checkpoint                  TEXT NOT NULL,
    UNIQUE (task_id, event_sequence)
) STRICT;
