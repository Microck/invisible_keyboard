---
title: Verify operating-system input and compute paths
status: closed
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

## Resolution

Resolved in
[Operating-system input and compute paths](../../research/os-input-and-compute-paths.md).
The lowest-friction bench route is a local desktop decoder feeding Linux
`uinput`; the cross-platform product candidate is a decoder on the wearable or
a dedicated gateway feeding USB HID first and BLE HOGP second. A raw sensor
transport plus replaceable desktop decoder remains the research route.

The report verifies platform boundaries rather than assuming API parity.
Windows `SendInput` cannot cross UIPI or secure desktops, Windows VHF adds a
signed kernel driver, and macOS virtual HID is entitlement-gated. Android can
use external HID or a selected IME. iOS custom keyboards cannot serve secure or
phone-pad fields and may be rejected by apps, so full-scope iOS needs external
HID. Browser-only output is confined to a cooperative web app. A target-app
verification matrix ties all routes to the ten cumulative feasibility gates.
