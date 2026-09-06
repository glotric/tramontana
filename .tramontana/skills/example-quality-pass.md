---
id: example-quality-pass
name: Example quality pass
description: Demonstrates the portable skill format without enabling behavior.
version: 1
enabled: false
tags: [example, quality]
required_capabilities: [filesystem.read, process.run, git.read]
---

# Purpose

Validate a scoped implementation change.

# Procedure

1. Read the repository instructions and relevant manifests.
2. Inspect the current diff and keep unrelated user changes intact.
3. Run the narrowest applicable formatter, linter, type check, and tests after permission evaluation.
4. Summarize changes, checks, failures, and remaining risks.
