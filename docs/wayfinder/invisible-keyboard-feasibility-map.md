---
title: Invisible keyboard feasibility
status: closed
labels:
  - wayfinder:map
---

# Invisible keyboard feasibility

## Destination

An implementation-ready, source-backed feasibility dossier for finger-driven
keyless text entry. It compares credible sensing architectures, recommends a
first prototype and fallback, defines hardware, software, data, evaluation,
cost, privacy, safety, licensing, and operating-system requirements, and makes
every preference-dependent assumption visible before implementation begins.

The destination is reached when the human can approve one falsifiable prototype
plan without needing another architecture-discovery pass.

## Notes

- Use the [feasibility dossier](../research/invisible-keyboard-feasibility-dossier.md)
  as the standalone research handoff and conditional prototype recommendation.
- Reload the `wayfinder`, `domain-modeling`, and `grilling` skills at the start
  of every planning session.
- Use [CONTEXT.md](../../CONTEXT.md) as the shared vocabulary.
- Treat the [local evidence orientation](../research/local-evidence-orientation.md)
  as a source map, not as externally verified proof.
- Verify claims that affect architecture selection against primary sources.
  Record the user, session, task, layout, and metric boundary for reported
  performance.
- Keep intent detection, symbol decoding, language correction, and operating
  system event injection conceptually separate even when a later implementation
  combines them.
- During the initial research pass, human decisions remained open while the
  user was away. The later product, sensing, decoder, calibration, and
  prototype instructions resolved them; the linked tickets retain the record.
- Keep this effort in the local Markdown tracker. Do not create GitHub issues
  or publish external artifacts without a separately confirmed draft.
- This map covers feasibility and prototype specification. It is not a software
  implementation plan.

## Decisions so far

- [Confirm the first invisible-keyboard use case](tickets/001-confirm-first-use-case.md)
  fixes the product boundary: a wearable-only, personal calibrated system for
  familiar split-QWERTY tapping on passive unmarked surfaces. The physical
  reference has 34 positions, while the first advertised manifest is Core-28:
  A-Z, Space, and Backspace.
- [Select the first sensing architecture](tickets/009-select-sensing-architecture.md)
  selects LapCal's five finger IMUs plus one dorsal reference IMU per hand as
  the hard topology reference. The implementation follows the documented
  independent-implementation/no-artifact-reuse policy, uses wired acquisition
  first, and permits cameras or instrumented surfaces only as laboratory truth.
- [Choose the decoding and personalization strategy](tickets/010-choose-decoding-strategy.md)
  selects a causal staged decoder: physical channel identifies the finger,
  relative finger-to-dorsal motion supplies intent features, and a personal
  per-finger classifier identifies the position without a language model.
- [Specify the decisive prototype and evaluation protocol](tickets/011-specify-decisive-prototype.md)
  selects a USD 77.05 one-hand electronics build around XIAO nRF52840 Plus,
  TCA9548A, and six LSM6DS3TR-C breakouts. A 60-minute stream gate precedes the
  second hand, and the one-hand rig separately tests contact observability. It
  cannot create an armable Core-28 artifact. Guided keyboard-label calibration
  and locked passive-surface validation begin only after both hand rigs pass
  admission.
- [Audit what LapCal actually proved](tickets/002-audit-lapcal-evidence.md) finds useful
  hardware and data-collection lessons, but no reproducible proof of key
  identity, live text entry, or product-level performance; treat it as a
  cautionary baseline, not the implementation base.
- [Compare sensing architectures for keyboardless finger input](tickets/003-compare-sensing-architectures.md)
  finds conditional co-leads: dual-wrist high-rate IMUs are the cheapest
  passive-surface test when personal calibration and language decoding are
  allowed; egocentric optical tracking leads when a camera/headset and its
  privacy cost are acceptable. Radar and magnetic tracking remain gated
  fallbacks. The later user decision selects the finger-IMU topology for the
  product despite its greater key-identity risk because environmental sensing
  violates the type-anywhere boundary.
- [Map prior art, products, and abandoned attempts](tickets/004-map-prior-art-and-failures.md)
  finds a
  fundamental product fork: finger-sequence systems can achieve high phrase
  throughput by letting language infer same-finger keys, while arbitrary
  spatial QWERTY still needs geometry or an instrumented surface. Gest is the
  only confirmed shutdown; other silent projects remain status-unknown.
- [Define feasibility metrics for usable text entry](tickets/005-define-feasibility-metrics.md)
  requires cumulative evidence from stream integrity through continuous raw and
  corrected text, transfer, ordinary-movement negatives, and extended use; it
  keeps language-assisted phrases, exact arbitrary keys, and learned accessible
  or chorded input as separate claims.
- [Verify operating-system input and compute paths](tickets/006-verify-input-and-compute-paths.md)
  makes Linux `uinput` the bench route and standards USB HID the cross-platform
  appliance route; mobile software paths remain target-specific, with hard iOS
  secure-field limits and no browser-only system keyboard path.
- [Cost viable prototype paths](tickets/007-cost-prototype-paths.md) finds a
  USD 95-180 host-decoded dual-wrist IMU rig is the only new-hardware sensing
  experiment that comfortably fits the historic EUR 200 cash ceiling; owned
  optical hardware can also fit, while labor, participants, and higher-risk
  modalities require separate, much larger budgets.
- [Map privacy, safety, licensing, and intellectual-property constraints](tickets/008-map-risk-license-and-ip.md)
  keeps both co-leads feasible only behind local-data, explicit-capture,
  fail-neutral input, human-safety, and pre-market gates; LapCal is study-only
  for the canonical implementation, and the bounded patent search is not FTO.

## Not yet specified

None. All child tickets have a linked resolution. The remaining unknowns are
physical execution gates, not planning questions.

The implementation contract is maintained in the
[passive-surface reference architecture](../implementation/reference-architecture.md),
the [first prototype roadmap](../implementation/first-prototype-roadmap.md),
and the [guided calibration contract](../implementation/calibration-experience.md).

## Out of scope

- Building, manufacturing, releasing, or marketing the production system in
  this planning run.
- Powered exoskeletons, rehabilitation hardware, force feedback, and clinical
  claims.
- Generic speech, eye-tracking, or brain-computer text entry that does not infer
  finger typing gestures.
- Treating an offline classifier score, a model binary, or a hardware render as
  proof of usable text entry.
- Copying, adapting, publishing, or commercializing LapCal code, PCB, CAD, data,
  or models in the canonical implementation absent a separate rights grant.
