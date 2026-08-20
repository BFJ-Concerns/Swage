# The Build Ledger (shared format)

The obligation ledger is the shared control plane of the two build phases:
the structural phase seeds it, the assembly run discharges it, and both
write this one format. On disk it is two things, each short enough to read
whole: the header file `build/ledger.md`, and one record per obligation in
`build/obligations/<slug>.md`. Status is **derived from recorded evidence,
never asserted** — a State whose required companion field is empty reads as
the weaker status, whatever it says.

A record holds only its current truth; git holds its history. Update a
record by overwriting what changed — the State line, the Evidence pointers,
a sharpened Must-be-true — never by appending a dated layer beneath the old
text: a layered record hands a bounded read whichever layer it lands on,
while `git log -p build/obligations/<slug>.md` already returns the whole
supersession chain on demand. Prose narrating how the record reached its
current state is transcribed git history: delete it. What the past still
teaches — an approach tried and found wanting, a constraint discovered the
hard way — is current knowledge: keep it, in the present tense, as part of
the record's fields.

## The Header (`build/ledger.md`)

Current values only — a superseded ref is replaced in place, never chained
behind `previously` clauses:

```markdown
# Ledger — <project>
- **Phase base:** <commit hash of the tree before structural work began>
- **Handover ref:** <branch or commit assembly builds from — under a PR
  merge policy the structural branch; the literal
  word `unset` until handover — the assembly's opening check reads this
  exact word as "structural phase still open">
- **Promotion PR:** <PR-mode only: the number of the standing WIP
  promotion PR (structural branch → main) the structural phase opened at
  handover; absent under direct-merge>
- **Last audit:** <path of the most recent audit report and its window
  `<base>..<head-hash>`, or `not applicable — no teardowns`, or `none`>
```

## The Obligation Record (`build/obligations/<slug>.md`)

```markdown
# <obligation slug>
**State (YYYY-MM-DD):** <status> — <one short clause when the bare word underdetermines it; keep the whole line on ONE physical line — the status board reads single lines>
- **Must be true:** the outcome, stated verifiably — what exists and works
  when this obligation is discharged, in the commission's terms. Always
  the product outcome: "an exemplar covers this" is the `needs-spine`
  status, never the obligation itself — worded that way, the record comes
  true the moment the exemplar lands and tells the build run nothing
  about what to build.
- **Proof:** the `VERIFICATION.md` rung that discharges it, named
  concretely (the journey, the command, the consumer exercised), and the
  tree it runs against — the unit's own tree, a merge preview (lane tip
  plus unit head or heads; the assembly skill's landing discipline
  defines it), or the lane only where the work is already merged onto it
  (direct-merge mode, or an estate proof after landing) — a unit proof
  under PR mode never names the lane, which would re-create the
  prove-needs-merge deadlock the preview exists to break. This obligation's own discharge proof, never a phase's
  verification model, and never the project's whole-tree check gate —
  the gate is shared regression assurance with its own fixed venues
  (the assembly skill's landing discipline), and a gate written here
  re-runs at every stage that honours the Proof line; a Proof line
  repeated identically across records has
  stopped carrying information. Proof is run only by the phase that
  discharges the record: the structural phase covers an obligation without
  running its proof, so a test named here stays red until the assembly's
  unit lands.
- **Source:** where it came from — the commitment ID (or commission line,
  for a pre-register commission), the teardown sweep
  record, the estate journey, the triage-wave proposal.
- **Lane:** converted queue items under PR mode only — `main` (the work is
  independent of the structural phase's reshaping and lands as an
  ordinary reviewed PR against main) or `structural`, judged at
  conversion (the assembly skill's landing discipline). Absent reads as
  `structural`.
- **Evidence:** the pointers the status requires (empty otherwise) — the
  current pointers only, replaced as they are superseded. An `in-flight`
  record with an open PR also carries the PR number and its unmet de-WIP
  conditions here (the assembly resume sweep reads them).
```

The dated State line opens every record, rewritten in place at every status
change, so the status board is one mechanical call with no index file to
hand-maintain (the directory is the index; a hand-kept one is one more
record that goes stale):

```sh
grep -H '^\*\*State' build/obligations/*.md
```

A reader who reads one record top to bottom holds its complete current
truth; nothing below corrects it.

## Status Vocabulary

- `open` — no unit in flight; the resting state.
- `needs-spine` — no exemplar covers the work. Written by either phase (the
  assembly run when admission fails; the structural phase at handover, to
  name a shortfall it leaves uncovered); cleared **only by the structural
  phase** — rewritten to `open` in the same edit that records the
  covering exemplar in `build/spine.md`, or, where that phase authored
  the deliverable outright instead of covering it, to `built-in-phase`.
- `in-flight <unit-slug>[, <unit-slug>…]` — written at dispatch, one slug
  per live unit; a replacement unit's slug replaces a dead unit's in the
  same edit that journals its death.
- `built-in-phase` — the structural phase completed the obligation's
  whole work (an outright-complete exemplar, or instruction text, which
  only that phase authors) and it is on the handover ref, unproven —
  that phase runs no proofs. Written only by the structural phase, with
  Evidence pointing at the landing commit in the same edit; empty
  Evidence reads as `open`. The assembly run discharges it with
  proof-only work — run the record's named rung, rewrite to `proven` on
  the evidence — never a build unit.
- `parked: <what>` — where `<what>` is either `decision <question>`
  (waiting on the user — the question was delivered to them, and their
  answer is a decision to act on) or
  `blocked-on <mechanism>` (waiting on a named mechanism; its recovery is
  not a decision).
- `proven` — requires Evidence in the same edit: pointers to the proof run
  (the command and its recorded output, the estate result, the verdict
  file). `proven` with empty Evidence reads as `open`.
- `waived <by> <date>` — requires Evidence pointing at the user's recorded
  say-so (their answer to the delivered question, or their words quoted
  from the conversation). There
  is no self-waiver; `waived` citing anything but the user reads as `open`.

## Rules

- Obligations are edited only to sharpen truth, never to match what a unit
  happened to build; a result suggesting the obligation is wrong is a
  contract question for `build/gaps.md`, and the record parks.
- Units reference obligations, never the reverse: the unit folder records
  which obligation its brief serves; a failed unit changes nothing here but
  the in-flight marker.
- One capability, one record: before creating a record, search
  `build/obligations/` for it — a restoration obligation seeded at teardown
  and a triage-wave proposal for the same capability are the same record,
  merged at landing.
- An obligation whose Source is a `TASKS.md` item carries that item with
  it: whichever seat rewrites the record to a terminal state — `proven`
  or `waived` — deletes the item from
  `TASKS.md` in the same edit, the commit message carrying the
  disposition; a record merged away under one-capability-one-record
  repoints the item's `(→ <slug>)` marker at the surviving record in the
  merging edit. The queue holds only work still owed.
- The run-close **assembled-whole estate result** is ledger material too:
  a standing record slugged `assembled-whole`, created by the assembly run
  at open (Must be true: the assembled product proves out as a whole;
  Proof: the closing proof estate the project's `VERIFICATION.md` places
  at run close — the full end-to-end walkthrough of the assembled product
  where the project has a user-facing surface, plus the estate-level
  proofs deferred here — naming the ref it ran against), proven only at
  run close. The Proof rule above binds this record like any other: the
  whole-tree check gate discharges nothing here. It exists so run
  completion is itself a derived status rather than a journal claim. It
  is also the home for estate-level proofs no single unit can run (the
  assembly skill's landing discipline).
