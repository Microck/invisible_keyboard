---
title: Choose the decoding and personalization strategy
status: closed
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

## Resolution

The user delegated the conservative layout and implementation design. The
canonical decoder is staged and causal: direct sensor identity supplies the
finger, relative finger-to-dorsal motion supplies intent features, and a
per-finger position classifier selects only among that finger's enabled
positions. The first baseline is a regularized nearest-centroid classifier with
personal calibration, confidence rejection, no language model, and whole
sessions held out across complete remounts.

Core-28 supports discrete taps only. Reserved positions, low-confidence
decisions, simultaneous candidates, and unsupported event semantics abstain
instead of becoming a nearby key. See the
[passive-surface reference architecture](../../implementation/reference-architecture.md).
