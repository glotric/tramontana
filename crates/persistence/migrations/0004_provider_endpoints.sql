-- Model providers the user configured on this machine.
--
-- Profile-scoped, with no `project_id`: `CONFIGURATION.md` §153 forbids
-- project-authored Markdown from hard-coding a provider or embedding a
-- credential, and a project that could name its own endpoint would carry a
-- routing decision into every checkout of it.
--
-- `config` holds the variant-specific payload as JSON. The alternative is a
-- column per field of every provider shape, most of them null, and a
-- migration every time a shape gains one. What this table is asked is always
-- "give me every endpoint" — nothing filters on a base URL — so the queryable
-- columns are the ones an interface lists and orders by, and the rest is a
-- document. See `docs/decisions/0011-provider-settings-store.md`.
--
-- No credential value is storable here: the contract type carries a
-- `SecretRef`, which is an opaque key and a store name, so what lands in
-- `config` cannot contain a secret (`SECURITY_AND_PERMISSIONS.md` §7).
--
-- There is no `location` column. Whether an endpoint is local or cloud is
-- derived from what it points at, because a row that could *declare* itself
-- local would defeat Local Only, which is a hard egress prohibition.
CREATE TABLE provider_endpoint (
    endpoint_id    TEXT NOT NULL PRIMARY KEY,
    display_name   TEXT NOT NULL,
    enabled        INTEGER NOT NULL,
    kind           TEXT NOT NULL,
    config         TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    schema_version INTEGER NOT NULL
) STRICT;

-- Listing is ordered so two runs show the same thing and a test can compare
-- rather than sort.
CREATE INDEX provider_endpoint_by_creation
    ON provider_endpoint(created_at, endpoint_id);
