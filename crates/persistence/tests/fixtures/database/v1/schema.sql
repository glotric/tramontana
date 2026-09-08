CREATE TABLE _sqlx_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    success BOOLEAN NOT NULL,
    checksum BLOB NOT NULL,
    execution_time BIGINT NOT NULL
);

INSERT INTO _sqlx_migrations (
    version, description, installed_on, success, checksum, execution_time
) VALUES (
    1,
    'initial',
    '2026-01-01 00:00:00',
    1,
    X'd1f1a26b4f0b3d3a49915be669b7e4e6f1936779ed04f6eaab3a92f979fded5934aef90f5d8b77ef2d73b5d238477ccc',
    0
);

CREATE TABLE project (
    project_id   TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    created_at   TEXT NOT NULL
) STRICT;

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

INSERT INTO project (project_id, name, created_at)
VALUES ('fixture-project', 'Fixture project', '2026-01-01T00:00:00.000Z');

INSERT INTO project_root (project_id, canonical_path, granted_at)
VALUES ('fixture-project', '/synthetic/project', '2026-01-01T00:00:00.000Z');

INSERT INTO conversation (
    conversation_id, project_id, title, created_at
) VALUES (
    'fixture-conversation', 'fixture-project', 'Fixture conversation',
    '2026-01-01T00:00:01.000Z'
);

INSERT INTO task (
    task_id, conversation_id, project_id, goal_summary, state,
    last_sequence, created_at, updated_at
) VALUES (
    'fixture-existing-task', 'fixture-conversation', 'fixture-project',
    'Existing fixture goal', 'queued', 1,
    '2026-01-01T00:00:02.000Z', '2026-01-01T00:00:03.000Z'
);

INSERT INTO task_event (
    task_id, sequence, event_id, conversation_id, occurred_at, kind,
    visibility, correlation_id, schema_version, payload
) VALUES (
    'fixture-existing-task', 1, 'fixture-event', 'fixture-conversation',
    '2026-01-01T00:00:03.000Z', 'taskCreated', 'user',
    'fixture-correlation', 1,
    '{"kind":"taskCreated","goalSummary":"Existing fixture goal","conversationId":"fixture-conversation"}'
);
