# Campaign State

All campaign state lives in the annexe, under `build/qa/` (sharing the
`build/` root with the build phases, whose ledger this campaign reads —
and writes only to package findings as obligation records, or to reopen
obligations in a leakage hand-off) and `harness/` (the durable
instrument fleet, which outlives any one commission). Read this at
campaign open.

## Directory Inventory

- `build/qa/campaign.md` — the campaign record: candidate sha and the
  freeze note, the landed test map with each entry's risk tier and
  exercise disposition (an instrument proposal, its decision, and any
  open venue riding the entry), the spend budget
  (per quota pool, in its declared unit) with measured spend-to-date
  by pool and dispatch class, pointers to the run records it was
  summed from, and the per-pool close-out reserve, the staffing table
  (per dispatch class, with its charter-tier variations), the
  product-session width with its declared basis and any other width
  bound with its evidence, entry-gate result, charter status, halted
  charters, declared downgrades (black-box exceptions, staffing
  overrides). The record a resume reads first.
- `build/qa/workflows/` — campaign-authored workflow scripts, where a
  wave's shape genuinely departs from the skill's carried `driver-wave`
  and `judgement-wave` (the default dispatch machinery, which needs no
  copy here): kept beside the record so a resume finds the campaign's
  own machinery (run inline from the file, or saved to the space),
  reading its staffing from the campaign record's table.
- `build/qa/coverage.md` — the durable coverage ledger, kept across
  campaigns (references/campaign.md, *Carried Coverage*): per map
  entry, the seat-derived surface set, the evidence pointer, the sha,
  and the outcome — plus the queued masked-path entries packaging
  writes. Updated as evidence lands, not at the close.
- `build/qa/checks/<date>.md` — the seat's direct-check batches: each
  check's entry or finding, the command or step, and the result — the
  evidence a `checked` coverage entry cites.
- `build/qa/findings/<slug>.md` — one record per finding
  (references/findings.md owns the format and state machine); the
  campaign's only authority on what is known and owed.
- `build/qa/charters/<slug>/` — per charter: the charter itself, plus
  one dated `<session>/` directory per dispatch holding that session's
  step record, captures, oracle cards, flags, and the judgement
  records. Imported and committed by the seat from each session's
  isolated workspace (references/charters.md) — black-box workers never
  hold the annexe, and imported evidence is never overwritten:
  the overwrite-in-place rule governs records, and evidence a finding
  cites must survive its session's later rejection.
- `build/obligations/qa-<finding-slug>.md` — packaged findings, written
  once at routing (references/findings.md, *Packaging Findings for the
  Build*) and discharged by the build phases, not this campaign.
- Open questions to the user (taste batches, budget extensions,
  standing decisions — delivered through the campaign's routing) are
  campaign state too: the campaign record lists them, and every wake
  and resume sweeps for answers.
- `build/qa/leakage/<date>.md` — leakage reports
  (references/campaign.md, *Leakage and the Hand-Off*).
- `build/qa/report.md` — the closing report with the merge
  recommendation. Writing it is what closes the campaign: its absence
  beside a campaign record marks a campaign to resume.
- `build/qa/journal/<campaign-start>.md` — append-only narrative
  (`YYYY-MM-DDTHHMM` local); never read for state.
- `harness/<instrument>/` — the instrument, its calibration record, and
  its run evidence (references/instruments.md).

## Record Discipline

Records hold current state only, overwritten in place — git holds their
history. Journal events before the next sleep, not at wrap-up. Commit
annexe files as they change, staged by path, and push annexe commits as
they land. The campaign resumes from artefacts alone: a resumed seat
reads `campaign.md`, sweeps the finding states against the world (a
`routed` record's obligation package exists where the record says; an
`awaiting-ruling` record's question is still open or has answers to
apply), reconciles toward the evidence, and continues from what it
finds.
