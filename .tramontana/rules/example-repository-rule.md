---
id: example-repository-rule
name: Example repository rule
description: Demonstrates a scoped native rule.
version: 1
enabled: false
priority: 100
applies_to: ["apps/desktop/src/**"]
---

Keep UI code focused on presentation and interaction. Place routing, permissions, secrets, persistence policy, and provider behavior behind the Rust core boundary.
