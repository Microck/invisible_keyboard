---
title: Define feasibility metrics for usable text entry
status: closed
labels:
  - wayfinder:research
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by: []
---

# Define feasibility metrics for usable text entry

## Question

Which measurements and test designs distinguish a useful invisible keyboard
from an offline gesture classifier?

Establish source-backed definitions and evaluation protocols for at least words
per minute, character and word error, substitutions, missed activations, false
activations during typing and ordinary movement, gesture-to-event latency and
jitter, correction load, calibration time, learning curve, session transfer,
user transfer, fatigue, and task completion. Specify how to report press,
release, modifier, repeat, and rollover behavior. Identify benchmark text sets
and study designs suitable for a small prototype without overstating
generalization.

Research may propose threshold ranges and explain their basis. Final acceptance
thresholds remain a human decision tied to ticket 001.

## Resolution

Resolved in the
[usable text-entry feasibility contract](../../research/usable-text-entry-feasibility-contract.md).
The contract separates language-assisted phrase transcription, actual arbitrary
key intent, and optional accessibility or chorded entry. It defines immutable
session data, split-before-windowing evaluation, raw and corrected text metrics,
continuous false-activation exposure, latency and operating-system behavior,
transfer, learning, comfort, privacy, power, and recovery evidence.

Ten cumulative stage gates now make a prototype falsifiable, from full-topology
stream integrity through extended use. Metric definitions and protocol
boundaries are source-backed; absolute usability, false-activation, transfer,
fatigue, setup, and battery stop/go values are explicitly provisional until the
human chooses the product claim and acceptance target.
