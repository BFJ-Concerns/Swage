---
name: Assembly
description: Run the mass-build phase of a commissioned project — deliver the implementation volume over the structural spine the maestro phase landed, dispatching builders and their verification through templated cross-family pipelines and tracking completion as proof obligations rather than prose status. Use when the user wants to start, resume, or check on a build run for a project whose spine and ledger exist, to build out the remaining commission work at volume, to operate a run already in flight (dispatch, steer, or replace units, verify and land their output, discharge proof obligations), to drain the annexe task queue's journey items through a run, or to close a run out and settle its ledger. Not for structural or spine work (that is maestro, and units without a covering exemplar route back to it), not for capturing or changing a commission (the commission skill), and not for a single self-contained task an agent could just do.
---

# Assembly

You keep a fleet of pipelines moving; the spine has already settled the
architecture. The seat is deliberately not the scarce top tier — a top-tier
session invoked here says so and offers to hand the seat down. One run
serves one commission, and the run is done when the obligation ledger is
discharged — every record proven or explicitly waived by the user — never
when the units merely stop. The run's close is the QA campaign's open: a
drained ledger hands the frozen lane to the shakedown skill (*Handing Off
to the Campaign*, below) rather than merging promotion itself.

## Start From a Current Clone

Before reading the ledger, `git fetch` the annexe and the code repo:
current, proceed; behind and clean, `git pull --ff-only`; behind and dirty,
surface it and ask; diverged, stop and surface it; remote unreachable, say
currency could not be verified and ask. Leave live worktrees alone — each
holds a unit in flight.

## The Ledger Is the Run

The ledger — the header `build/ledger.md` and one record per obligation in
`build/obligations/` (in the annexe, shared with maestro) — carries
**obligations**: things that must be true and proven before the build is
complete, each with the proof it requires per the annexe's
`VERIFICATION.md` consumer ladder. Read completion from recorded evidence;
write it only when the evidence exists. Units are disposable tactics
against obligations — a failed unit is redispatched, reshaped, or split
without touching the obligation it serves, and an obligation is never
edited to match what a unit happened to build. Prose status anywhere else
is narrative, not state.

Every run — fresh or resumed — opens with **reconciliation**
([references/ledger.md](references/ledger.md)): the handover check first (a
header whose handover ref is `unset` is a structural phase still open, not
a run to start; under PR mode the ref is the structural branch, the base
every unit builds on), then every non-terminal status verified against the
world. No `build/` directory at all is a structural phase not yet run, and
no commission `README.md` sits a step further back still: stop and name
the workflow that owes the missing artefact — maestro for the spine and
ledger, a commissioning conversation for the contract — rather than
improvising either here.
A fresh run then derives the obligation set by **triage wave** (same
reference): cheap workers propose the decomposition — criteria first, which
vary by project; rarely by-file — and you judge and land the result. The
wave proposes; it never commits.

## Read Records Whole; Check the Tree Before Concluding

A bounded read licenses no conclusion. A windowed read of a record, or a
heading grep over a document, returns whichever slice it happened to land
on — a heading list is not a read — so read a record to its end before
quoting its state, and treat what a bounded read did *not* show as unknown,
never as absent. Before asserting that work is missing, blocked, or
unbuilt, check the live tree for it directly — including the half a
document does not cover: that an old surface is still wired proves nothing
about whether its replacement exists. A dated or tree-pinned document — a
research inventory, a review report, a triage survey — is a measurement at
its named ref, not present truth, however timelessly it is phrased. When
writing such a document, open it with the ref it measures, and state
findings as measurements — a survey never issues ledger statuses or
standing instructions.

## Admission Is Binary

A unit is dispatchable when its brief names the spine exemplar it extends
(`build/spine.md`) and the obligation it serves. Admission is settled at
your seat, before any unit artefact exists: before creating a worktree or
writing a brief, read the candidate exemplar's spine entry and judge the
fit — the exemplar is in the inventory, and the files the unit will touch
sit inside its **Extends at** envelope without crossing **Imitation stops
at**. No exemplar fits → the unit is not built here at all: mark its
obligation `needs-spine` and hand it to maestro (*Maestro Is a Dispatch*,
below) — the spine inventory is a whitelist, and improvising outside it
is how builders imitate what nobody assessed. Builders read anything, modify and imitate
spine only. Do not dilute this gate under schedule pressure. **Estate and docs units are the exceptions**: their
deliverables are tests and documentation, not product code. An estate brief
names its obligation and ladder rung, and carries `Extends:` only when a
test-pattern exemplar exists to name; a docs brief names its obligation and
the landed units it covers (*Documentation Rides the Run*).

Two classes never reach a builder at all — check `built-in-phase` first.
An obligation marked **`built-in-phase`** already has its deliverable on
the handover ref; dispatch only its proof — run the record's named rung,
record the evidence, rewrite to `proven` (for instruction text, whose rung
is reading-based, the verification legs are the whole discharge). An
obligation whose deliverable is **instruction text** — an agent prompt, a
model-steering document, anything future sessions execute unsupervised —
and that is *not* so marked is the structural phase's unfinished work,
not work awaiting an exemplar: mark it `needs-spine` and hand it to
maestro (*Maestro Is a Dispatch*), whatever the spine contains. A build unit against
either is the misroute: one builds what already exists, the other hands
the volume tier an artefact class it must not author.

## Maestro Is a Dispatch

A `needs-spine` record is not a question for the user: it is maestro's
work, and maestro needs no user input to do it — the commission, the
ledger, and the spine are all on disk. So a run that marks a record
`needs-spine` dispatches maestro itself, at the moment the marking
lands, rather than parking the record until a person notices. The
dispatch is a delegated child (the `delegate` tool, `completion_mode`
detach, `skills: ["maestro"]`, the project tree as `cwd`) briefed with
exactly what a re-entry needs: the annexe path, the `needs-spine`
record slugs, and the instruction to extend the spine, hand back through
the handover ref, and rewrite the records to `open` — nothing more, since
maestro reads its own situation from `build/`. One maestro child at a
time: a second `needs-spine` marking while one is running joins that
child's work list on its next re-entry, never a parallel structural seat
on the same lane.

Maestro's hand-back is an event for this run: its records return to
`open` and the handover ref advances, so the next wake's reconciliation
finds them as ordinary open work. The seat that dispatched maestro takes
no structural decision itself — it names the gap and reads the result.
The exception stays the user's: a `needs-spine` record that a maestro
re-entry sends back unchanged, or that maestro's report says needs a
commission decision, is surfaced to the user as a parked record, not
redispatched.

## Pipelines Carry the Ceremony

Every code-bearing dispatch runs as a templated pipeline — one of the
workflows this skill carries (`build-verify`, `estate-unit`,
`docs-update`, `triage-wave`), started by name — never as a bare
worker: the
templates make verification a stage the script reaches rather than a
decision anyone makes, and landing waits for verdicts by construction.
A pipeline's holds arrive as `decision.needed` events on its run's
queue; you answer by posting the decision event back into the run, and
an unanswered decision blocks that unit while the rest of the fleet
moves.
(A direct fix dispatches nothing and is the sized exception —
references/landing.md, *Direct Fixes*.) Your
job at the seams is judgement on the outputs: read
[references/pipelines.md](references/pipelines.md) before the first
dispatch and [references/briefs.md](references/briefs.md) before the first
brief.

- **Builders** ride the build family's flagship tier at low effort (the
  brief and exemplar decided the path); fix rounds step the builder up to
  medium — a verdict that sent the unit back falsifies that ground.
  **Verifiers** are cross-family,
  below their family's top tier, judging brief-fit, commission-fit,
  overbuild, and whether the unit's proof could fail — a test satisfiable
  by a broken product is a severe finding, in the standard pipeline as
  in estate. The scarce top tier appears nowhere in these pipelines.
- **Severe findings converge before you hold**: the pipeline autonomously
  fixes and re-verifies a severe-bearing verdict, up to three rounds, so
  the triage hold usually presents an already-fixed unit; a blocked fix
  or a severe finding surviving its round escalates early
  (references/pipelines.md, the fix rounds stage).
- **The restatement opens every unit** and the pipeline holds until you
  accept the sketch — misalignment is cheapest at minute five.
- **Verdict findings at the hold are triaged, not obeyed**: a finding that
  collides
  with the contract routes to `build/gaps.md` and the obligation parks;
  one the record never addressed becomes a fix unit; severe findings each
  take an explicit disposition, sub-severe findings left undispositioned
  at a land are accepted into the annexe issue log (the envelope's
  `issues_findings` — file them when recording the landing); every
  explicit disposition is journalled.
- **Waivers are the user's alone**, asynchronously: park the unit and
  put the question to the user — a delivered message stating what cannot
  be proven and why; build other work while it waits. Parks have a
  return path — sweep parked records on every resume and wake; the
  user's answer or a resolved gap rewrites the record to `open`,
  journalled.
- **UI-bearing units get fresh first-encounter eyes**: the brief carries
  the concrete proof obligations (drive the UI, screenshot key states,
  leave scripted tests where regression value exists) and names where
  the visual evidence lives — the unit directory,
  `build/units/<slug>/` — and that it survives the round: a screenshot
  judged by its author and tidied away reduces to the author's word,
  which is what the verify stage exists to check. Before release
  a fresh session — briefed on the commission's intent slice only, never
  the brief or report — judges the result as the user's first encounter.
  Fresh *context* is the property this check claims, so it runs on the
  build family (driving and judging a live UI is that family's strength —
  its strong mid tier, not the flagship); cross-family independence is
  the verify stage's job, not this one's. The semantic verifier checks
  conformance; conformance to a brief nobody would enjoy using is what
  fresh eyes catch.

## Sizing at Dispatch

Before dispatching an obligation, answer two independent questions, in
order:

1. **Fan-out: how many units?** Does the work divide into slices kept
   apart by a clean seam? Judge from the current tree; treat any triage
   fan-out proposal as advice, and judge even when none was proposed.
   Separable slices dispatch as parallel sibling units
   (references/briefs.md, *Sibling Briefs*); slices whose edits would
   collide serialise. The obligation stays open until every sibling
   lands and its single proof runs over the joined work.
2. **Provability: is each unit sized right?** A unit is as large as one
   consumer proof can cover — no single proof, split it; trivially
   satisfiable proof, it is a rider, not a unit. A rider-shaped
   remainder with no host — the last work on its obligation, nothing
   in flight or planned to carry it — dispatches as a unit anyway,
   its sizing journalled honestly: "rider, not a unit" is sizing
   honesty, never a licence for code-bearing work to skip the
   pipeline.

A capability never lands dark: every unit that builds a capability
includes at least its thinnest real consumer, so the unit's own proof can
run through a product-reachable path — a capability with no consumer has
no proof any rung can discharge, and its PR waits on work that is not in
it. Where the full consumer estate exceeds one unit, the first real
consumer rides with the capability and the remaining consumers are their
own obligations. This composes with the brief's surface glance
(references/briefs.md): a consumer briefed alongside the capability it
consumes passes the glance by the unit's own charter, so the two rules
together bar exactly two shapes — a consumer briefed against a surface
that neither exists nor is being built, and a capability unit with no
consumer in its own scope — and never the pair building as one. Where that consumer sits under a different exemplar's
envelope, the brief names both exemplars and admission judges each edit
against the envelope covering it — or the pair runs as serialised
siblings under one obligation, whose single proof runs at the consumer
and whose capability sibling never lands ahead of its consumer sibling,
so the composition keeps the no-dark-capability bar; differing envelopes
never license landing dark. The corollary governs tests: a test lands in the unit
that makes it pass, never ahead of it — a unit PR red on tests awaiting
future capability has imported unbuilt work into its own gate.
(Legitimately-red restoration tests seeded by the structural phase on the
structural branch are the documented exception; they are ledger-owned red,
not unit red.)

Conflating the questions dispatches a monolith whose separable slices
cannot land or park independently.

## Queues and Finds

`TASKS.md` is the decided-journey queue, and this run drains its journey
work. Each item carries its kind as a trailing tag, assigned by
commission triage: `(journey)` items are yours; `(structural)` items are
maestro's — the tag is the routing, so take no disposition on them
(the shared deletion rule still applies when you flip any converted
record to `proven`, whatever its source item's tag); an untagged item
is awaiting triage — take it only when it is plainly journey-shaped and
small, otherwise leave it for commission. Disposition is a
reconciliation step, ordered before the width choice
(references/ledger.md, *Reconciliation*): at every open, dispose of
every journey item and every untagged item you take, so the width
judgement counts what the queue yields. An item already marked `(→ …)` is dispositioned already —
it rests until its record's terminal flip. Judging an item takes one of
four routes — each route taken leaves its trace on the item's own line
in the same edit (the journal is never read for state) — or leaves the
item deliberately open:

- **Batch** it, when the item is a small fix — and most queue items
  are: the queue accumulates small decided work, and small decided work
  is burnt through, not ceremonialised. A small behavioural fix — too
  real for a glance, too small to earn its own unit — converts into the
  run's open **fixes-batch obligation** (references/landing.md, *The
  Fixes Batch*): an ordinary ledger record that collects such fixes,
  citing each item as a Source, with the item marked
  `(→ <batch-slug>)` in the same edit and deleted at the record's
  `proven` flip like any conversion. A mechanical repair within the
  direct-fix bar skips even that: land it as a direct fix
  (references/landing.md, *Direct Fixes*) and strike the item in the
  landing commit.
- **Convert** it into its own obligation record citing the item as
  Source, when the work is genuinely unit-sized — through the same
  admission gate as any other obligation (`needs-spine` included) — and
  mark the item `(→ <obligation-slug>)` in the same edit.
  Under PR mode, set the record's `Lane:` as you convert
  (references/build-ledger.md): `main` when the work is independent of
  the structural phase's reshaping, `structural` otherwise
  (references/landing.md, *PR-Mode Topology*).
- **Strike** it, when its work already landed — check against `proven`
  records, then delete the item, the commit message carrying the
  disposition.
- **Mark it blocked**, when a named dependency — an obligation, a user
  decision, an external fact — stops it: `(blocked on <blocker> since
  <date>)`, the marker coming off the moment the blocker clears.

Busyness is not a route: an item the run cannot reach yet is left
open, unmarked — a legitimate judgement, recorded only in the opening
digest's remainder list, and the queue seat's standing work (below).
An item that is the wrong seat's
work is never parked — structural-shaped work stays for commission
or maestro by its tag, and a journey item failing admission routes
`needs-spine` as ever.

**One seat belongs to the queue.** While the queue holds an open,
unblocked journey item no seam excludes, one of the run's seats — the
queue seat — works queue-sourced work: the fixes-batch unit, the
converted queue obligations, oldest first by the items' `(added
<date>)` stamps unless a seam dictates otherwise. It takes other
obligation work only when no queue item is workable — queue empty,
wholly blocked, or every open item seam-excluded — and returns to the
queue as soon as one is. The seat counts against width exactly as any
unit does (a builder working its tree; references/ledger.md, the width
step): the reservation is a fill-order preference inside the width the
machine already granted, never a governor — it never idles a seat and
never caps width. It exists because a queue competing with obligations
on equal terms loses every time: each item is individually small, so
each is individually never urgent, and the queue only grows.

At open, re-check each blocked item's named blocker — cleared means the
marker comes off and the item is open work — and flag to the user any
item carrying a tag or marker the protocol does not define. Name the
open and blocked remainder to the user at run open and run close — the
items themselves, each with why it remains, never a count — alongside
the queue header's `Last triage:` date (`none`, or no header, means no
commission pass has yet re-judged the queue). The
open is not complete
until every journey item has been judged — reading the queue is not
judging it.

From
`ISSUES.md` (unverified claims), take a find only when its meaning is
plain, clearly journey not destination, self-evidently worth doing, small,
and still real against the current code — all five; on any doubt leave it
for commission triage. A find this run takes is deleted from the log in
the commit that takes it. Workers' finds route through their reports; you
append them to `ISSUES.md` — never to `IDEAS.md`, the user's pen alone.

## Estate and the Assembled Whole

Per-unit proof composes into nothing without journeys across units. Estate
work is first-class ledger material: obligations for the commission's
cross-unit journeys, discharged by units whose deliverable is the tests
and their passing runs. An estate unit's tests must fail against the
baseline and pass with the change — a test that cannot fail proves
nothing. The `assembled-whole` record also carries the estate-level proofs
no single unit can run (references/landing.md, *Proving Against a Merge
Preview*). It is proven only at run close, by the closing proof estate the
annexe's `VERIFICATION.md` places there — open that file at close and
discharge each proof it defers to this point. Chief among them is the
**full end-to-end walkthrough** of the assembled product: a
fresh-context worker walks each commitment at its named consumer's own
boundary, briefed on the signed commitment lines verbatim — never an
outcome summary — and every commitment is judged **present, absent, or
inconclusive** against its falsifier at your seat after the walk, with
anything but present blocking close. (A pre-register commission still
awaiting its migration substitutes its What prose, verbatim, for the
register.) The dispatch contract, evidence rules, and the legacy
handling are references/ledger.md, *Closing the Run*. The whole-tree check gate
is regression assurance, never this proof. The tree it proves is main
under direct-merge, the structural branch tip under PR mode, named in the
evidence; the walkthrough's dispatch contract is references/ledger.md,
*Closing the Run*.

Duplication against landed work is the verify stage's standing question,
not an uncovered residue; the gap these nets accept is siblings still in
flight — and any landed sibling a verifier's fetch could not yet see. A
mechanical duplication sweep — enumerate helper-shaped definitions,
cluster near-identical bodies, report an extraction worklist for the
annexe issue log — can be commissioned on demand as cheap grunt-tier
work whenever accumulation warrants it; it is never a close gate.

## Handing Off to the Campaign

Under PR mode the promotion PR merges only after the QA campaign has
judged the assembled product — the campaign tier of the ladder is the
shakedown skill's to discharge, not this run's. So run close ends with a
hand-off, not a merge: freeze the lane, take the estate result, journal
the close with the frozen tip named, and dispatch the campaign as a
delegated child (`delegate`, detach, `skills: ["shakedown"]`, the project
tree as `cwd`) briefed with the annexe path and the frozen sha and
nothing else — the campaign reads its handover from the ledger. The
campaign's merge-ready close is what merges promotion; a leakage close
returns the branch here, reopening the affected obligations, and the
next wake's reconciliation picks them up as open work. A run whose
ledger drains while a campaign is already open on the same lane does not
dispatch a second: it journals the new tip and lets the open campaign's
moved-tip handling decide.

## Documentation Rides the Run

Landed work drifts the repository's documentation: a unit that changes
the product's usage surface — user-visible behaviour, commands,
configuration, APIs — leaves README and docs/ claiming what the tree no
longer does. Documentation documents using the product (the project-docs
skill's scope): a change to development process alone — release
mechanics, internal restructuring that alters no usage surface — raises
no docs obligation.
Documentation is ledger material like anything else — when landed work has
changed documented reality, raise or extend a docs obligation and
discharge it through the docs pipeline (`workflows/docs-update.js`): a
writer working through the project-docs skill over the landed changes,
verified cross-family against the tree (references/pipelines.md,
docs-update). The annexe's `docs_policy` knob (config.toml) names which
registers the project maintains — `both` (the default when absent),
`agent`, `human`, or `none`, which disables docs obligations — so raise
them accordingly. A docs unit is sized to its one writer: the pipeline
funnels every covered unit's changes through a single writer's context
(the project-docs skill's authorship is deliberately single-writer), so
a unit covers several landed units only as far as one writer can absorb
them. Dispatch a docs unit whenever accumulated drift approaches that
limit — several per run is the normal shape, colliding doc edits
serialising like any other siblings (*Sizing at Dispatch*) — never one
end-of-run batch, which overloads the writer and serialises run close
behind it. Under any policy other than `none` the run still does not
close on stale documentation, but that is a currency check, not the
discharge point: check the maintained registers' currency alongside the
assembled-whole proof. Changelog upkeep is
each unit's own, never deferred to this pipeline — the brief carries the
expectation and the unit's verifier checks it (references/briefs.md).

## Landing

Landing stays inside the pipeline, per the annexe's `merge_policy` and
`push_policy`. The one class that skips the pipeline is the **direct
fix** — a tiny mechanical repair landing on the structural branch without
a pipeline or PR; the boundary and its proof are references/landing.md,
*Direct Fixes*. Docs units complete their full pipeline, then merge
directly into their lane — never by PR (references/landing.md, *Docs
Units Land Direct*). A unit whose work builds on a sibling's unmerged
branch opens its PR targeting that branch — dependency is PR targeting,
and chains land bottom-up (references/landing.md, *Stack Rulings*). In
PR mode the `WIP:` prefix is a two-way latch whose
removal is yours alone, gated on the obligation proven — on the unit's own
tree or a merge preview of lane tip plus unit head, never waiting on the
merge itself — every severe finding dispositioned, the accepted sub-severe
residue filed to the annexe issue log, and no dependent able to force
a change; a de-WIPed PR
belongs to the autonomous PR-review system, never merged by you or a
worker. The full latch discipline — the gate-venue doctrine, the merge-preview
proof, API
readbacks, PR state in the ledger,
the resume sweep, forge dependency wiring, run-close disposition of every
WIP PR — is [references/landing.md](references/landing.md): read it at the
first landing and at every de-WIP, re-block, or run-close PR decision. Its
run-level guard (pin verification) applies after every pipeline. Parallel units run in worktrees, one per unit, in
the `<project>-Worktrees/` sibling; check free disk and quota before each
dispatch — build CPU and memory are not dispatch inputs; they are
ungoverned, and enter at the width judgement through the seat's glance
at the machine, not per dispatch (references/ledger.md, the width
step).
Resource pressure trims
concurrency and returning headroom
widens it, never the sizing answer (*Sizing at Dispatch*). Re-take the
run's width at reconciliation and each wake (references/ledger.md,
*Reconciliation*), and maintain it between them: when a unit leaves
active build, fill the freed seat from the ready work — an open
obligation, or a queue conversion — at that event, not
at the next reconciliation (what counts as active build:
references/ledger.md, the width step). A unit's review chain is sequential
within that unit alone; it never paces unrelated dispatches.

## Quota Is Observed, Never Assumed

Every routing decision reads allowance state as a fact with provenance — a
refusal event, a provider limit response, the user's word —
and its reset time; a stale exhaustion is re-read once its reset passes.
Near a ceiling, step down inside the same family or park; never silently
drop a pipeline's verification legs to save quota. When a whole family is
out, the cross-family property is unavailable: park what needs it and tell
the user, the question riding a delivered message if they are away.

## State and Resilience

Run state lives in the annexe's `build/` directory, shared with maestro:
`ledger.md` and `obligations/` (the run's only authority; `needs-spine`
written here, cleared
only by maestro), `spine.md` / `retired.md` / `teardowns/` / `audits/` /
`reviews/` (maestro's records, read-only here), `research/` / `triage/`
(dated measurement documents, each opening with the ref it measured),
`gaps.md` (contract questions, drained by commission passes),
`journal/<run-start>.md` (append-only narrative;
`YYYY-MM-DDTHHMM` local; never read for state), and `units/<slug>/`
(brief, restatement, holds
and decisions, verdicts, evidence pointers). Records hold current state
only, overwritten in place — git holds their history (the annexe rule
carries the discipline; references/build-ledger.md applies it to the
ledger). Journal events before the
next sleep, not at wrap-up; commit annexe files as they change, staged by
path. The run resumes from artefacts alone.

**The seat is a fresh segment, not a session.** A run left unattended is
steered by a standing schedule (the `schedule` tool: session `fresh`,
this space, an agent whose definition activates this skill, an interval
of ten to twenty minutes, gated on a cheap model whose criteria name the
events worth a wake — a pipeline holding for a decision, a run that
ended, a worker that died, allowance state that changed). Each fire is a
new session with no transcript: it opens by reconciliation
(references/ledger.md), reads the open runs and their pending holds
(`workflow_run_status`), clears the whole batch of decisions at once
(`run_event_post`), fills freed seats from the ready work, journals what
it did, and ends. Nothing carries between segments except the annexe's
records and the run records — so every decision is written to the
record before the segment ends, or the next segment re-litigates it. An
attended seat may hold a conversation open and self-wake instead; the
discipline is the same, only the transcript differs. Arm the schedule at
run open, before the first dispatch, and remove it at run close.

Run events land in the record first; chat is a digest derived from it,
never carrying anything the record does not. A chat message is triggered
by its content, not by a wake firing or a tool result arriving: a decision
only the user can make, a finding that changes the run's picture, a
landing, a change in the run's own state (blocked, paused, degraded,
closing) — a unit moving between its pipeline's stages is the run
working, not a state change — or the user asking, which always licenses a
reply. Decisions come first, ranked, each
with a pointer into the record. A wake that finds none of these produces
no message — still-running, still-healthy, and not-yet-finished are not
events, and absence of a message means the run is proceeding. When the
run is unattended, a notification carries the same digest at the same
triggers.

## References

- [references/pipelines.md](references/pipelines.md) — the pipeline
  templates: stages, tier pins, the hold protocol and decision schemas,
  estate proof modes, and watching a run. Read before the first dispatch.
- [references/build-ledger.md](references/build-ledger.md) — the shared
  ledger contract: header, the per-obligation record format, and status
  vocabulary. Read at run start, before references/ledger.md.
- [references/ledger.md](references/ledger.md) — this run's ledger use:
  reconciliation at every open (the handover gate), the shared build
  cache, the triage wave, and closing the run (the walkthrough contract).
  Read at run start, and again at run close.
- [references/briefs.md](references/briefs.md) — brief, restatement,
  report, verifier-findings, and PR-body shapes. Read before the first
  brief.
- [references/landing.md](references/landing.md) — the WIP latch, gate
  venues, merge-preview proving, PR-state
  ledger discipline, forge wiring, and the run-level guards. Read at the
  first landing.
- `workflows/` — the pipeline templates (`build-verify.js`,
  `estate-unit.js`, `docs-update.js`, `triage-wave.js`), carried by this
  skill and started by name.
