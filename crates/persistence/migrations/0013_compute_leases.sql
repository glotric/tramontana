-- What rented compute did and cost (ADR 0072): started, ready, stopped and
-- why, each with its rate. Append-only; a row is one event's JSON.
CREATE TABLE compute_lease_events (
    sequence  INTEGER PRIMARY KEY AUTOINCREMENT,
    lease_id  TEXT NOT NULL,
    event     TEXT NOT NULL
) STRICT;
