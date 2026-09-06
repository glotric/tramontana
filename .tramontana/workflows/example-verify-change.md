---
id: example-verify-change
name: Example verify change
description: Demonstrates a portable workflow definition.
version: 1
enabled: false
inputs:
  - name: scope
    required: false
required_capabilities: [git.read, process.run]
---

1. Read the effective repository instructions.
2. Inspect the selected change scope and current diff.
3. Propose the relevant quality commands and request permission where required.
4. Run approved checks with time and output limits.
5. Summarize results without exposing secrets or unrelated file content.
