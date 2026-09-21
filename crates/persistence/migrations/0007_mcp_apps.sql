-- MCP Apps: servers Tramontana starts on the person's configuration
-- (ADR 0036).
--
-- `config` holds the program's full path, its arguments, its working folder,
-- the environment variables it is given by value, and the secrets it is given
-- by reference. A reference names an entry in the operating system's
-- credential store; no secret value is ever written here (ADR 0008).
--
-- App configuration belongs to the profile, not to a project: it is not part
-- of a project export, and importing a bundle never starts an App
-- (`PROJECT_FORMAT.md` §5).
CREATE TABLE mcp_app (
    app_id         TEXT NOT NULL PRIMARY KEY,
    display_name   TEXT NOT NULL,
    enabled        INTEGER NOT NULL,
    config         TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    schema_version INTEGER NOT NULL
) STRICT;

-- Listing is ordered so two runs show the same thing.
CREATE INDEX mcp_app_by_creation ON mcp_app(created_at, app_id);
