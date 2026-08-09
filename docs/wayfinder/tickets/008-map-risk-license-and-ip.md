---
title: Map privacy, safety, licensing, and intellectual-property constraints
status: closed
labels:
  - wayfinder:research
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by: []
---

# Map privacy, safety, licensing, and intellectual-property constraints

## Question

Which constraints could invalidate, restrict, or materially change an invisible
keyboard architecture before implementation?

Cover keystroke and motion-data sensitivity, raw-data retention, telemetry,
model inversion or replay, wireless attack surface, host permissions, wearable
battery and skin-contact safety, radio and product compliance boundaries,
dataset consent, and accessibility claims. Determine the practical effect of
LapCal's CC BY-NC-SA 4.0 license on code, PCB, CAD, trained artifacts, and a
future commercial product. Map close patent families and distinguish landscape
research from a legal freedom-to-operate opinion.

Use official licenses, statutes or regulator guidance, standards material, and
primary patent records where available. State jurisdiction and uncertainty.

## Resolution

Resolved in
[Privacy, safety, licensing, and IP constraints](../../research/privacy-safety-licensing-and-ip-constraints.md).

The two conditional co-leads remain feasible only behind privacy, safety, and
fail-neutral input gates. The dual-wrist IMU path must keep motion, inferred
text, and correction state local by default; the optical path adds a hard
bystander/environment-capture review. Both must pass deterministic key-state,
least-privilege, all-up recovery, password/logging, and explicit research
capture controls before system input or participant data collection.

LapCal is study-only for the canonical project. Its repo-wide CC BY-NC-SA 4.0
statement blocks commercial copying/adaptation absent a separate grant, the
dataset/model participant and provenance rights are unproved, and the license
grants no patent rights. The report specifies a fresh clean-room implementation
route. The targeted patent list is not an FTO opinion; counsel must chart the
frozen architecture's live claims, families, status, ownership, and territories.

FCC/RED/GB market access, Bluetooth qualification, USB mark/VID use, body-worn
RF, battery, electrical, material, ergonomics, and accessibility work become
product gates after intended use, components, claims, and markets are fixed.
No new ticket was created because the open architecture, decoder, and prototype
decisions already own those determining facts.
