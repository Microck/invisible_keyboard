---
title: Compare sensing architectures for keyboardless finger input
status: closed
labels:
  - wayfinder:research
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by: []
---

# Compare sensing architectures for keyboardless finger input

## Question

Which sensing architectures can recover intended finger typing gestures and
key identity with enough evidence to justify a prototype?

Compare at least wearable inertial sensing, bend or strain sensing, contact or
force sensing, capacitive sensing, optical hand tracking, acoustic or vibration
sensing, radar or ultrasound, surface electromyography, and useful hybrids.
Evaluate air typing and generic-surface typing separately. For each option,
record demonstrated task, user and session scope, symbol count, false
activations, misses, substitutions, latency, drift, occlusion, calibration,
fatigue, power, mechanical burden, privacy, current component availability,
and evidence quality. Prefer primary papers, official technical material, and
reproducible projects over product claims.

The result must expose which modalities directly observe intent or contact and
which must infer it from motion alone.

## Resolution

Resolved by the
[sensing architecture comparison](../../research/sensing-architecture-comparison.md).
It ranks a TapType-style dual-wrist IMU prototype and an egocentric optical
prototype as conditional co-leads, documents the evidence and failure boundary
for every required sensing family, defines defensible hybrids, and carries the
LapCal audit's acquisition and evaluation gates forward. The human-only final
architecture choice remains open on the parent map.
