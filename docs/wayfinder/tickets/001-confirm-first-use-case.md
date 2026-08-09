---
title: Confirm the first invisible-keyboard use case
status: closed
labels:
  - wayfinder:grilling
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by: []
---

# Confirm the first invisible-keyboard use case

## Question

Which exact use-case profile should all recommendations optimize for?

The decision must state:

- what "invisible" excludes and whether a glove, rings, wrist module, generic
  surface, camera, or environmental sensor is acceptable;
- the first physical setting and target platform;
- whether the user expects QWERTY muscle memory or accepts a new chorded or
  reduced layout;
- the minimum symbol, modifier, shortcut, repeat, and rollover behavior;
- whether a personal calibrated prototype is acceptable before cross-user use;
- the build budget, acceptable setup time, and tolerance for visible hardware.

This is a human decision. Do not resolve it from the historic under-EUR-200
robotics conversation, because the current request does not restate all of
those constraints.

## Resolution

The user resolved the product boundary on 2026-08-09. The product must type on
passive surfaces with no surface sensors and should use LapCal's inexpensive
wearable topology as its hard reference. Familiar split-QWERTY assignments are
preferred, with a conservative first manifest and a defined expansion path.

The exact contract is in the
[passive-surface reference architecture](../../implementation/reference-architecture.md).
The first target is a personal calibrated, two-hand, desk-contact prototype on
Linux. The physical geometry has 34 positions; the first advertised manifest
is Core-28: A-Z, Space, and Backspace.
