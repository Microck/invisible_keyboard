---
title: Specify the decisive prototype and evaluation protocol
status: closed
labels:
  - wayfinder:grilling
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by:
  - 010-choose-decoding-strategy.md
---

# Specify the decisive prototype and evaluation protocol

## Question

What is the smallest staged prototype program that can falsify the chosen
architecture before expensive mechanical or software work begins?

Define each stage's hypothesis, hardware and software boundary, participant and
session scope, data and label path, task, baseline, measurements, success and
stop thresholds, artifact output, cost, and next decision. The stages must
progress from intent or contact timing to key identity, continuous text,
ordinary-movement false activations, operating-system injection, and a short
wearability trial without using later stages to excuse a failed early one.

The final protocol must make clear what result authorizes the build-ready
specification and what result returns the project to the fallback sensing
architecture. Human approval closes the feasibility map.

## Resolution

The decisive build starts with one wired hand: a plain XIAO nRF52840 Plus, a
TCA9548A mux, and six identical LSM6DS3TR-C breakouts with direct data-ready
lines. The selected electronics total USD 77.05; the realistic one-hand
planning envelope is USD 227 before tax and tools. The one-hand rig must pass
Gate 0A and may then run a non-armable contact pilot that only decides whether
to expand. Fund and admit the second rig before the canonical two-hand Gate 0B
round. The one-hand pilot cannot pass Gate 0B or produce production Core-28
readiness by itself.

The staged program separates stream integrity, contact, acting finger, exact
key identity, live text, remount/ordinary-motion exposure, and wearability.
Each stage has explicit scope, metrics, stop rules, artifacts, and fallback.
Shared SPI is the electrical fallback if the same sensors fail I2C timing. The
wearable exact-input claim stops if same-finger positions remain unobservable
after stream, mount, and time-alignment quality pass twice.

Calibration uses normal-keyboard key events for a cheap personal warm start,
then requires a short passive-surface transfer and locked validation. It never
asks the user to measure switch travel or keyboard angle and cannot loop
forever toward a false 100% state.

See the [first prototype roadmap](../../implementation/first-prototype-roadmap.md),
[calibration experience](../../implementation/calibration-experience.md), and
[wire protocol v2](../../implementation/wire-protocol-v2.md).
