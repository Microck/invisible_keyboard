---
title: Choose the decoding and personalization strategy
status: open
labels:
  - wayfinder:grilling
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by:
  - 009-select-sensing-architecture.md
---

# Choose the decoding and personalization strategy

## Question

What sequence of decisions should turn the chosen sensor stream into text while
keeping false activations, training burden, and correction behavior acceptable?

Choose among direct key classification, staged intent and key decoding,
finger-zone decomposition, position or trajectory inference, chords, temporal
models, and bounded language correction. Decide which parts require personal
calibration, which must transfer across sessions or users, where inference
runs, and how confidence, abstention, correction, press, release, modifiers,
repeat, and rollover behave. Identify the simplest baseline that can disprove
the need for a more complex model.

This is a human decision informed by the selected sensing architecture, not a
license to hide weak sensing behind unrestricted language-model autocorrection.

