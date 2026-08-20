# Running the Ledger

How this run uses the ledger — the header `build/ledger.md` and the
per-obligation records in `build/obligations/` — the run's only authority
on what is done. The layout, record format, status vocabulary, and the
rules they hold are the shared contract in
[references/build-ledger.md](references/build-ledger.md): both build phases
write that one format, so read it first. This file carries what is specific
to the assembly run — deriving the obligation set, and reconciling on
resume.

Two of the shared rules bear repeating because this run is where they bite:
`waived` is written only on the user's recorded say-so — there is no
self-waiver; and `needs-spine` **routes, it does not queue** — marking it
means telling the user maestro has work, and only maestro rewrites it to
`open` (in the same edit that lands the covering exemplar), so those
records are maestro's re-entry list, never a backlog this run absorbs.

## The Triage Wave

Opening a fresh run, the wave proposes the obligation set — from the
commission-versus-codebase delta and from `TASKS.md` (pass `tasksPath` and
`obligationsDir`, both required),
whose remaining items the workers propose over; disposition stays your
call at landing, per the queue disposition (SKILL.md, *Queues and
Finds*) — a small item the fix route will burn through needs no
obligation, so drop proposals the queue routes already cover rather than
expecting the wave to know your dispositions. Dispatch cheap
workers over that ground with two jobs: propose
the decomposition criteria first (by capability, by journey, by surface —
whatever cleaves this project's remaining work at its joints; by-file is
almost never it), then the obligations under those criteria, each with its
suggested proof rung, source line, and fan-out — the independent slices
the obligation's work divides into, with the seam that separates them, or
none when the work is one piece. Fan the wave so proposals overlap —
two workers covering the same slice from different criteria surface the
decomposition disagreements you need to see.

The wave proposes; you land. Judge criteria before entries — a wrong
decomposition makes every record under it wrong together. Check each landed
obligation against the commission line it cites (re-read, never recall),
and against `build/retired.md` so a retired capability does not re-enter as
an obligation. Where the commission carries a commitment register, the
landed set is not settled until it covers the register: map every
commitment and goal ID to a proposed obligation, an already-proven
capability, or a
`needs-spine` routing, with each `stands on` entry resolving inside the
same mapping — a goal line resolves through its `stands on` set plus the
closing walkthrough that exercises its falsifier, never through an
umbrella obligation duplicating its substrate — then check the mapping
blind, dispatching a cheap worker
the bare lists — register IDs against covered IDs, and the register's
`stands on` pairs against the mapping's resolutions — and taking back only
the
symmetric differences, since a checker handed your reasoning alongside the
lists rationalises a mismatch and an ID-only diff misses a register that
kept its lines and lost its dependency structure. An unmapped commitment becomes an
obligation, a `needs-spine`, or a `build/gaps.md` question in the landing
edit — never a silent absence. Record the whole mapping — the entries
resolved to already-proven capabilities and the `stands on` resolutions
included — in the wave's proposal document, so a resume or the close can
audit coverage rather than re-derive it from memory. Fan-out is dispatch advice retained in the proposal
document, never ledger material — judge it afresh at dispatch (SKILL.md,
*Sizing at Dispatch*). The landed ledger is
yours: the wave's output is a proposal document at
`build/triage/<run-start>-proposal.md` (pass that as the workflow's
`outPath`), kept for the audit trail, never the ledger itself.

## Reconciliation (Every Open)

These checks open every run — a fresh start as much as a resume: the fresh
run is exactly the session that meets a paused structural phase. Derived
status means the opener trusts evidence over assertions — including the
previous session's. Before dispatching anything:

1. **Check the layout, then the handover.** An annexe carrying a legacy
   `maestro/` directory, or whose `build/ledger.md` still holds `##`
   obligation sections, is a pre-split
   layout: migrate it first (references/build-ledger.md, *Migrating*;
   SKILL.md, *State and Resilience* for the legacy directory) before
   reading anything as state. The ledger header must carry a handover
   ref that exists (under PR mode the structural branch, with the
   `Promotion PR` line naming a PR the forge confirms is open,
   `WIP:`-prefixed, and headed by the structural branch with main as its
   base — an open WIP PR with the wrong head or base is not the
   promotion handle), and a `Last audit` line naming a report
   (or `not applicable — no teardowns`, acceptable only while
   `build/teardowns/` is empty). A ledger with no handover ref is a
   structural phase paused, not a run to start — tell the user maestro is
   still open. Do not require the handover tree to build or run green:
   it arrives unexecuted by design, restoration obligations open and
   their tests legitimately red, and function is established here per
   unit as obligations discharge. Read the audit report and weigh what
   it says; its discrepancies are information for this seat's judgement,
   not a door.
2. **Baseline with the project's own gate and adopt what it finds.**
   The baseline is measured with the project's **full gate** — the
   whole-tree suite recipe behind its declared gate entrypoint, in the
   gate-venue terms of references/landing.md, *Gate Venues* — never the
   cheap check gate, and never a subset a seat assembles: a baseline
   taken with a weaker instrument reads clean
   while the gate's first stage fails, and every later unit trusts
   that word when deciding whether a failure is its own. This run owns
   every inherited failure the gate finds, regardless of who caused
   it. On a fresh run, first make the handover
   tree able to answer: if it does not build or its gate does not
   execute, condition it — stubs and compile fixes as direct commits to
   the handover ref's branch, exempt from serving a named obligation
   (under PR mode see references/landing.md, *PR-Mode Topology*; the
   point is that later unit PRs are buildable and judgeable on their own
   diffs). Bring any compiler cache up before the first
   gate run (*The Compiler Cache*, below). Then run the full gate
   once against the conditioned handover ref and record the
   quantitative result — ref, command, per-stage outcome with pass and
   fail counts, and the names of the failures — as
   `build/triage/<run-start>-baseline.md`, green or red. A gate that
   halts at its first failing stage hides the stages behind it, so
   repair and re-run until every failure it still reports is one an
   open obligation owns. Seat-side repair in that loop stays
   mechanical — formatting-class, lint-autofix-class; anything larger
   becomes dispatched work, never unbriefed capability work at the
   seat. Sort the
   failures: one an open obligation already owns (a restoration
   obligation whose tests are legitimately red) stays with its record;
   the rest are this run's to clear — obligation records created now,
   sized like any other work (SKILL.md, *Sizing at Dispatch*: unrelated
   failures rarely share one proof), each citing the baseline as
   Source. On a resume, read the baseline file; re-run the gate only
   when the handover ref has moved.
3. Sweep the status board (`grep -H '^\*\*State' build/obligations/*.md`),
   then read each record whole — they are sized to be read whole, and a
   record's State line is only as good as its Evidence — and
   verify every status against the
   world: an `in-flight` unit's worktree and branch exist and match; a
   `parked` record's question to the user is still unanswered (an answer
   is a decision to
   act on); `proven` records' evidence pointers resolve (a preview proof's
   recorded lane sha included); a `waived` record's Evidence names the
   user's recorded say-so; a
   `built-in-phase` record's landing commit is reachable from the handover
   ref.
4. Reconcile disagreements toward the evidence — overwriting the record,
   never annotating it: a vanished worktree makes
   its obligation `open` again; a merged branch with no proof run stays
   unproven — schedule the proof, do not infer it.
5. Dispose of the decided-journey queue — every journey item, and every
   untagged item you take — before choosing width, so the width
   judgement counts what the queue yields rather than only what has
   already reached the ledger. The routes and markers are SKILL.md,
   *Queues and Finds*; most items are small, so most dispositions are
   fixes — the batch exists to burn them. Re-check each blocked item's
   named blocker (cleared means the item is open work again), and name
   the open and blocked remainder in the opening digest, item by item.
6. Choose the run's width — how many pipelines in flight, and why —
   from queue depth (open obligations plus what step 5 just
   converted), seam collisions, disk, quota, and what the machine can
   absorb; never inherit it from the last
   dispatch. The queue seat is filled from this width first: while
   `TASKS.md` holds an open, unblocked journey item no seam excludes,
   one seat works
   queue-sourced work — the fixes-batch obligation, the queue's other
   conversions — before other obligations claim the rest (SKILL.md,
   *Queues and Finds*). Heavy toolchain runs are ungoverned — nothing admits or
   throttles them — so the machine itself is the gauge: glance at free
   memory, swap, and load, and a machine deep in swap under concurrent
   builds pulls width down until it recovers. Units spend most of
   their life in inference, so width can run well past the number of
   simultaneous builds the box could bear — but only while their heavy
   executions stay staggered: builds and gates landing together are
   what saturates. Colliding seams still
   serialise, and demanding pipeline holds legitimately cap width;
   queue triage and dispatch bookkeeping never do — each is a couple
   of short searches and a decision, not an attention cost. Count a
   unit against the width only while a builder is actually working its
   tree: a unit in its verify stage, waiting at a hold, or parked
   holds your attention, not a build seat — and a hold decision that
   sends the pipeline back to building (a correction, a fix round)
   re-occupies the seat it freed. Width narrowed because reviews are
   slow is the width-decay failure, not a resource judgement.
7. Journal the reconciliation before the first dispatch: what the ledger
   claimed, what the world showed, what changed — and the width chosen,
   with its basis, the admissible gate concurrency included.

The same discipline closes the run (*Closing the Run*, below).

## Closing the Run

The ledger is discharged when every record is `proven` or `waived` — the
`assembled-whole` record proven on the closing proof estate, never the
check gate alone — and, under PR mode, the promotion PR is merged
(references/landing.md, *PR-Mode Topology*), with the closing journal
entry stating all of it, plus the queue's disposition — drained clean, or
the remaining items named one by one, each with why it remains; the same
list reaches the user in the closing digest, never a count. A run that
stops for
any other reason (allowance, user pause, blocked spine work) journals what
remains instead — the queue's open and blocked items named like the
close's — an honest open ledger, never a flattened one.

The closing proof estate is whatever the annexe's `VERIFICATION.md`
places at run close: open that file at close and discharge every proof it
defers to this point — mid-run deferrals of whole-product proofs all land
here, and this slot receiving them is what makes deferring them sound.
Chief among them is the
**full end-to-end walkthrough**: one worker walks the whole product as
its consumers, after unit-level proof is complete. The whole-tree check gate —
however green — exercises no journey, so it cannot stand in for the
walkthrough (the shared Proof rule, references/build-ledger.md).

The walkthrough's value is first contact:

- **Fresh context, the signed commitments verbatim.** Dispatch a fresh
  session briefed on the commission's commitment lines as written —
  IDs, consumers, and falsifiers included — and how to start the
  product; never unit briefs, reports, obligation records, this run's
  history, or a summary of outcomes. The verbatim lines matter twice
  over: a summary is the step in which an internal capability's absence
  disappears, and the falsifiers tell the walker what absence would
  look like. A pre-register commission still awaiting its migration has
  no lines to carry: brief its What prose verbatim instead — the
  no-summary rule holds either way. The brief carries the destinations,
  not the routes:
  knowledge of how the product was built fills exactly the gaps a real
  first consumer falls into, and the point is to measure those gaps.
- **Each consumer's own boundary, evidenced as that consumer sees it.**
  A commitment whose consumer is a person is walked through what a user
  would touch and judged from what the surface shows — a screenshot at
  each step of a graphical surface, the captured transcript of a
  terminal one. A commitment whose consumer is an agent, service, or
  subsystem is walked by driving that consumer's own interface — the
  tool call made, the API hit, the workflow invoked — and judged from
  what it returns. Source, internals, and logs stay closed during the
  walkthrough: a worker that reads the code to find the next step has
  stopped measuring what the boundary offers.
- **Being stuck is data.** A step the worker cannot find, a flow it
  completes only by luck, an error it cannot recover from — each comes
  back as a finding with severity and confidence, never debugged around.
  Triage the findings as any verifier's (fix unit, contract question,
  accept-with-note); friction that indicts usability rather than a
  defect is still a finding to route, not noise to drop.

Coverage is yours to reconcile, not the worker's to claim: after the
walk, check its step record against the register and the run's landed
obligations — the
worker cannot know what this run changed, by design. Judge every
commitment against its falsifier as **present, absent, or
inconclusive**: absent and inconclusive both block close, and
inconclusive is never rounded to present — a capability with no
obtainable discriminating evidence is a contract question for
`build/gaps.md`, not a pass. (Against a pre-register What, the same
three-valued judgement runs per claim the prose makes.) A commitment the
walk never exercised gets a follow-up leg (a further destination briefed
the same way), and the record is not proven until every commitment and
every behaviour the run added or changed is accounted for. The walkthrough's
evidence — the step record with its screenshots or transcripts, the
coverage reconciliation, and the ref it ran against — is what proves
`assembled-whole`, alongside whatever other estate-level proofs that
record carries. The deliverable is a report, not code, so the walkthrough
dispatches as a bare worker (references/pipelines.md, *The Template
Rule*) on the build family's strong mid tier — the fresh-eyes tier
(SKILL.md, *Pipelines Carry the Ceremony*): fresh context is the property
this proof claims, and cross-family independence stays the verify
stages' job.

## The Compiler Cache

A content-addressed compiler cache is the one cross-checkout build
sharing worth having: its keys are sound whatever tree the build runs
in, so worktrees and reclones reuse each other's compilation without
inheriting each other's artefacts. Wiring is per project — for Rust,
sccache via `RUSTC_WRAPPER=sccache`, with incremental compilation off,
which sccache requires — recorded in the annexe knob (`config.toml`):

```toml
[build_cache]
mechanism = "sccache"                # "none — <reason>" when assessed and declined
measured = "<date>: <primary-language hit rate, and how it was taken>"
```

An absent knob means not yet assessed, never declined — and assessment
means measurement, because a wired wrapper proves nothing about what
the cache returns. Run a build and record the hit rate
for the project's primary language from the cache's own statistics; an
aggregate rate across languages can read healthy while the primary
language sits at zero, so read the per-language figure. The knob
carries the measurement, so a later open can tell measured-and-working
from configured-and-assumed. Site the cache on a volume with room for
the run's working set and size it above that set: a cache permanently
at its ceiling is evicting what the run is about to reuse — read that
as misconfiguration, not health.

The boundary is what any cache or shared build directory returns. Caching the
production of artefacts — compilation, dependency fetches — is fair
however widely shared: the artefact is content-addressed or
fingerprint-keyed, and the tests still execute. Returning a stored
check result is not a run: any facet that would satisfy a test, lint,
or gate invocation from storage stays off for gate and proof runs,
because the run record and every ladder evidence pointer must come from
an execution on the tree they claim.
