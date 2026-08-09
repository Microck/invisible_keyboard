---
title: Verify operating-system input and compute paths
status: open
labels:
  - wayfinder:research
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by: []
---

# Verify operating-system input and compute paths

## Question

Which end-to-end output paths can turn decoded gestures into reliable text on
the likely target platforms, and where can decoding run?

Verify USB HID, BLE HID, a desktop host service, mobile input-method or
accessibility paths, and relevant extended-reality interfaces against current
official documentation. Record support for press, release, modifiers, repeat,
rollover, pairing, permissions, background execution, updates, and recovery.
Compare on-device, phone, desktop, and hybrid decoding for latency, power,
privacy, model size, logging, and development burden.

The result must identify a minimal prototype path and the platform choices that
would force a different architecture. Do not treat a visualization or serial
stream as keyboard integration.

