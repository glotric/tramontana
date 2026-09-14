-- Unknown model capabilities remain unavailable after upgrade.
ALTER TABLE provider_endpoint ADD COLUMN supports_tool_calls INTEGER NOT NULL DEFAULT 0 CHECK (supports_tool_calls IN (0, 1));
