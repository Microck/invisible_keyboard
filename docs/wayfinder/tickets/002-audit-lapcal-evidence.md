---
title: Audit what LapCal actually proved
status: open
labels:
  - wayfinder:research
assignee:
parent: ../invisible-keyboard-feasibility-map.md
blocked-by: []
---

# Audit what LapCal actually proved

## Question

Across main and every material unmerged ref, what parts of LapCal are
reproducible, what failed, and what can safely inform this project?

Produce an evidence table that covers hardware revisions, sensor topology,
sample rate, synchronization, firmware transport, dataset provenance, label
quality, train and test split, each reported metric, runtime integration, known
bugs, abandoned branches, and license obligations. Recompute important metrics
where the tracked data permits it. Distinguish isolated click detection from
key identity and complete text entry. End with a reuse classification for each
major artifact: evidence only, reusable design, repair candidate, or reject.

Start from `research/LapCal` and the branch leads in
`docs/research/local-evidence-orientation.md`. Do not treat generated model
binaries as reproducible results without their source data and training record.

