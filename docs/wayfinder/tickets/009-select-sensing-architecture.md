---
title: Select the first sensing architecture
status: closed
labels:
  - wayfinder:grilling
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by:
  - 001-confirm-first-use-case.md
  - 002-audit-lapcal-evidence.md
  - 003-compare-sensing-architectures.md
  - 004-map-prior-art-and-failures.md
  - 005-define-feasibility-metrics.md
  - 006-verify-input-and-compute-paths.md
  - 007-cost-prototype-paths.md
  - 008-map-risk-license-and-ip.md
---

# Select the first sensing architecture

## Question

Given the resolved product boundary and evidence matrix, which one sensing
architecture should the first prototype use, and which one fallback should be
preserved if its central hypothesis fails?

Present a ranked decision record with the strongest case for and against every
credible option. State what each sensor observes directly, what the decoder
must infer, which assumptions remain personal or session-specific, expected
cost and form factor, and the fastest falsification test. A recommendation may
be decisive, but architecture selection requires human approval.

## Resolution

The user selected a LapCal-style wearable as the hard reference and excluded
sensors in the surface. The canonical architecture therefore uses one IMU per
finger plus one dorsal reference IMU on each hand, per-hand local acquisition,
wired transport first, and local host decoding. Optical or instrumented
surfaces may provide laboratory truth but cannot influence product output.

The independently implemented design and the facts retained or rejected from LapCal are in the
[passive-surface reference architecture](../../implementation/reference-architecture.md).
The fastest falsification test is one hand through electrical stream integrity,
contact observability, acting-finger evidence, and a non-armable within-hand
position pilot. Exact Core-28 calibration remains a two-controller experiment;
the one-hand result decides whether buying the second rig is justified.
