# Local Wayfinder tracker

This directory is the local issue tracker for the feasibility effort. It exists
because creating GitHub issues requires a confirmed public draft, and no such
confirmation is available during the autonomous research run.

The map is a stable index of destination, settled decisions, fog, and scope. It
does not duplicate the open-ticket list. Each unresolved decision has one file
under `tickets/` with machine-readable frontmatter.

## Ticket fields

- `status`: `open` or `closed`.
- `labels`: exactly one Wayfinder work-mode label.
- `assignee`: blank means unclaimed. Research agents must claim an AFK ticket
  before working it and clear the field after closing it.
- `blocked-by`: ticket filenames whose decisions must close first.
- `parent`: the map that owns the decision.

Only tickets labeled `wayfinder:research` may run without the human present.
Tickets labeled `wayfinder:grilling` require an explicit human decision. No
agent may infer that decision from an old transcript or assign the ticket to the
user.

## Inspecting state

Use exact local queries instead of maintaining a second ticket list:

```bash
rg -l '^status: open$' docs/wayfinder/tickets
rg -l '^  - wayfinder:research$' docs/wayfinder/tickets
rg -n '^blocked-by:|^  - [0-9].*[.]md$' docs/wayfinder/tickets
```

