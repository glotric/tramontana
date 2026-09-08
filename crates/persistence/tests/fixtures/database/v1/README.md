# Database fixture v1

Synthetic SQL fixture representing a profile after migration `0001_initial.sql`
and before `0002_configuration_manifests.sql`. It contains one project with a
portable fake root and no personal data.

The fixture was authored from migration 0001 and records that migration's
SHA-384 checksum in SQLx's migration table. Its project, conversation, task,
and event prove that migration preserves existing state. Tests execute this
read-only SQL into a temporary database, then run the production migrator. Regeneration is
deterministic: update the copied schema and checksum only if a not-yet-shipped
migration is intentionally replaced.

License: repository MIT license; no third-party data.
