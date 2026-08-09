---
title: Invisible keyboard feasibility
status: open
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
- Human decisions remain open while the user is away. Research may narrow the
  options and recommend a choice, but it may not impersonate approval.
- This map covers feasibility and prototype specification. It is not a software
  implementation plan.

## Decisions so far

- The research target is finger-driven keyless text entry, based on the current
  request and the supplied LapCal reference. Unrelated text-entry methods such
  as speech, gaze, and brain-computer interfaces do not answer this question.
- Powered finger actuation and medical or rehabilitation devices are not
  required. The historic session asks for sensing, not force assistance.
- Local files and abandoned-project notes are leads. A claim is not considered
  current or general until the research pass verifies its evidence boundary.
- The autonomous run uses local Markdown tickets. It makes no public repository,
  GitHub issue, or external publication without a separately confirmed draft.
- [LapCal's evidence audit](tickets/002-audit-lapcal-evidence.md) finds useful
  hardware and data-collection lessons, but no reproducible proof of key
  identity, live text entry, or product-level performance; treat it as a
  cautionary baseline, not the implementation base.
- The [sensing architecture comparison](tickets/003-compare-sensing-architectures.md)
  finds conditional co-leads: dual-wrist high-rate IMUs are the cheapest
  passive-surface test when personal calibration and language decoding are
  allowed; egocentric optical tracking leads when a camera/headset and its
  privacy cost are acceptable. Radar and magnetic tracking remain gated
  fallbacks; the human architecture choice is still open.
- The [prior-art landscape](tickets/004-map-prior-art-and-failures.md) finds a
  fundamental product fork: finger-sequence systems can achieve high phrase
  throughput by letting language infer same-finger keys, while arbitrary
  spatial QWERTY still needs geometry or an instrumented surface. Gest is the
  only confirmed shutdown; other silent projects remain status-unknown.

## Not yet specified

- What "invisible" must exclude: visible overlays, a marked surface, any
  surface, a glove, rings, wrist hardware, or environmental sensors.
- First setting and platform: desk, lap, standing, mobile, desktop, or extended
  reality.
- Layout and output scope: QWERTY muscle memory, a reduced layout, chords,
  letters only, modifiers, symbols, shortcuts, and rollover.
- Whether the first success target may use personal calibration or must transfer
  between users and sessions.
- Acceptable budget, setup time, learning time, false activation rate, latency,
  text speed, correction load, fatigue, and privacy tradeoffs.
- Primary sensing architecture and fallback.
- Decoder, personalization, correction, and compute-placement strategy.
- Decisive prototype stages, measurements, and stop conditions.
- Exact component selection, enclosure, firmware, data schema, model, host
  service, input protocol, and procurement list.

## Out of scope

- Building, manufacturing, releasing, or marketing the production system in
  this planning run.
- Powered exoskeletons, rehabilitation hardware, force feedback, and clinical
  claims.
- Generic speech, eye-tracking, or brain-computer text entry that does not infer
  finger typing gestures.
- Treating an offline classifier score, a model binary, or a hardware render as
  proof of usable text entry.
- Reusing or publishing LapCal code or hardware before its license and technical
  fitness are resolved.
