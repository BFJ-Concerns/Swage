---
name: maestro
description: Run the structural phase of a commissioned build — bring the codebase to a build-ready state by establishing or replacing its architectural spine as landed exemplar code, with licence to tear out and rebuild what falls short. Use when the user wants to start, resume, or check on structural work for a commissioned project, prepare a repo for a mass build, author the instruction-text deliverables a commission names (agent prompts, model-steering documents — these are structural-phase work, never build volume), when a build run routes work back because no exemplar covers it, or when the annexe task queue holds structural-tagged items awaiting a phase. Not for the mass build itself (the assembly skill), not for capturing or changing a commission (the commission skill), and not for a single self-contained fix an agent could just do.
---

# Maestro

Run one commission's structural phase from the strongest judgement tier
available — when this session is not that tier, say so and stop rather
than doing structural judgement from a cheaper seat (a dispatched
re-entry that finds itself under-tiered ends with that in its report, so
the dispatching run can re-brief on the right tier). Establish only
the spine the commission's remaining work needs; feature volume belongs to
the assembly skill. The builders that follow extend proven patterns well
and improvise badly, so the phase's product is the **spine**: landed
exemplars and the inventory that names them.

## Start From a Current Clone

Before judging either repository, `git fetch` the annexe and the code repo:
current, proceed; behind and clean, `git pull --ff-only`; behind and dirty,
surface it and ask; diverged, stop and surface it; remote unreachable, say
currency could not be verified and ask. Apply the same gate to any repo
first used mid-run.

## Reading the Situation

No modes or flags — read the world:

- **No annexe or no commission** → stop and say which is missing;
  capture is the commission skill's job. An annexe whose `README.md` is
  absent — scaffold present or not — is an unsigned commission: name
  that state and its route (a commissioning conversation resumes the
  draft and lands the signature), and write nothing here.
- **Commission exists, no ledger header (`build/ledger.md`) in the
  annexe** → fresh phase: survey the codebase against the commission
  through dispatched recon (Grunt Work, below), judge the spine the
  remaining work needs, and begin. Structural state is the ledger header,
  never the bare directory: a `build/` holding other records — a
  commission's audit output, a stray file — is not a phase to resume, and
  nothing in it is the operator's to move or delete before this phase
  starts. Write the header there as the first act, beside whatever the
  directory already holds.
- **`build/ledger.md` exists** → resume or re-entry: read `build/spine.md`, the
  ledger header `build/ledger.md`, and the status board
  (`grep -H '^\*\*State' build/obligations/*.md`) first — the board
  locates records; it is not a read, so open the work list only after
  reading each `needs-spine` record whole. Re-entry from the
  assembly skill is the ordinary
  case — records marked `needs-spine` open the work list — and it
  ordinarily arrives as a dispatch: the assembly run delegates this
  skill the moment it marks a record, with the annexe path and the
  slugs, and reads the result from the ledger rather than from a
  conversation. A dispatched re-entry therefore needs no user in the
  loop: it takes its work list from the records, hands back through the
  handover ref, and ends with a report naming what it covered, what it
  left `needs-spine` with why, and any question only a commission
  decision can settle — the last two are what the dispatching run
  surfaces to the user. A
  `needs-spine` record's `Must be true` and `Proof` fields are the
  assembly's discharge criteria, never this seat's bar: covered means
  an exemplar the record's build unit would extend, judged by reading,
  and a failing test the record names stays red until that unit lands.
  Extend the spine to cover them and hand back **through the same channel as the
  original handover**: the new exemplar's commits reach the handover ref (a
  direct push to the structural branch under PR mode, a direct commit to
  main otherwise), and the
  header's handover ref advances to the ref that now contains them in the
  same edit that rewrites the records to `open` — an exemplar handed back
  on a ref the builders' worktrees will not contain is a silent failure.
  Where dependent unit PRs stand open against the branch being amended,
  the hand-back follows the validated path: the amended base runs the
  project's declared gate green at this seat before the push, and a
  rewrite is followed by the landing seat's restack cascade (the
  assembly seat's landing discipline, *Stack Rulings*). A
  re-entry that tore code out owes its own audit window; one that only
  added exemplars journals that no new audit is due.
- **Whenever the ledger header exists, sweep the commission before trusting
  that work list.** The ledger records only what was known when the spine was
  last judged sufficient, so a commission delta is invisible from
  `build/`: re-read the commission and the annexe's task list as they
  stand now — and where the commission carries a commitment register,
  sweep it by ID: every commitment and goal line, wherever it sits on the
  surface, and every `stands on` entry, since a dependency named there is
  contract material no heading may scope away. Work the delta implies — new
  obligations, new
  instruction-text deliverables — joins the work list; a delta may need
  new spine before the assembly resumes, and that is judged first.

**The queue's structural items are this seat's, read at the phase's two
boundaries.** `TASKS.md` items tagged `(structural)` route to this phase
even when no commission delta names them; no assembly run may absorb
them. Read the queue at phase open — fresh phase and re-entry alike —
and again before handover. Judge each structural item on its own
line, by one of three routes or a deliberate leaving-open: absorb it
into an obligation record citing
the item as Source —
new or existing, marked `built-in-phase` where this phase completes the
work outright — with the item marked `(→ <slug>)` in the same edit and
deleted at the record's `proven` flip (references/build-ledger.md,
Rules); strike it, when the tree or a terminal record already holds its
work — delete the item, the commit message carrying the disposition;
mark it `(blocked on <blocker> since <date>)` when a named dependency —
an obligation, a user decision, an external fact — stops it, the marker
coming off the moment the blocker clears; or leave it open — busyness
is not a route that marks anything, so an item this phase cannot
absorb simply stays open, and both
boundary reads name the open and blocked items to the user — the items
themselves, each with why it remains, never a count — alongside the
queue header's `Last triage:` date (`none`, or no header, means no
commission pass has yet re-judged the queue). An item this seat
judges mis-tagged or unplaceable is surfaced to the user. `(journey)`
items are the assembly's and untagged items are commission triage's —
leave both.

Read records whole, and check the tree before concluding: a bounded read —
a windowed slice of a record, a heading grep over a document — returns
whichever layer it lands on, so it licenses no conclusion, least of all a
negative one. Before asserting that work is missing or unbuilt, look in
the live tree for it directly. A dated or tree-pinned document — a recon
digest, a review report — is a measurement at its named ref, not present
truth; when writing one, open it with the ref it measures and state
findings as measurements, never as ledger statuses or standing
instructions.

## The Spine

Exemplar code is written at this seat — yours, in full, from design
through implementation. An exemplar is imitated at volume, with nothing
between it and its descendants to catch what review and tests catch in
ordinary code — so its authorship takes the strongest tier, overriding
the general tier tables: a table that routes code-landing-in-the-codebase
to a cheaper build tier is pricing the volume work, not the artefact the
volume work copies. Dispatching an exemplar's implementation is the
misroute, however settled its design. Tests are part of the pattern —
builders imitate test idiom as faithfully as code idiom — so author each
exemplar's tests with the same care, as code to be read and copied.

The project's toolchain is never run during the structural phase — no
builds or bundle rebuilds, no type-checkers or linters, no test suites,
no proof journeys, no running the product — at this seat or on a
dispatched worker. (Reading the repository is not a toolchain run;
a dispatched review or audit workflow stays within the rule only when
its brief carries it.) This stands even where project
guidance names its own gate — a verify recipe, a commit-time build or
lint step, a CI-parity claim: those conventions are written for
ordinary commits on a healthy tree, and this phase supersedes them for
its whole duration. Against a mid-demolition tree, toolchain output is
noise about work the assembly phase owns, and chasing it — a slide that
starts at the smallest permitted check — drags the phase into the
volume work it exists to avoid. Proving work at the project's real
consumer per the annexe's `VERIFICATION.md` ladder is the assembly
phase's per-unit bar, paid where execution is cheap. The one exception
is the closing gate run at handover (The Ledger and the Handover). This phase's
verification is reading: your own read-back of each exemplar, then the
blind spine reviews (references/spine-authoring.md) whose reports you
adjudicate before handover. A mechanically broken exemplar — even one a
type-checker would have caught — is caught at cheap-builder price by
the first unit that extends it, and routes back here as `needs-spine`.

## Instruction Text Is Spine Work

Deliverables that are instruction text — agent prompts, model-steering
documents, anything future sessions will execute unsupervised — are this
phase's work in full, never the assembly's: there is no
exemplar-then-imitate structure for them, because every copy must come
from the drafting tier, pattern and siblings alike. The commission
enumerates them, so author them all before handover — none trickle back
later. Write them in-seat by default; when the batch is large, dispatching
them is sanctioned only to agents on this seat's own tier and family, with
the commission slice and the prompt craft in the brief. An obligation this
authoring completes outright is marked `built-in-phase` in its record
(references/build-ledger.md), its proof left for the assembly run. The
same marking applies wherever an exemplar
completes its obligation's whole work: small enough work is finished by
its own example, and an unmarked completion sends the assembly a builder's
worth of work that does not exist.

Record each landed exemplar in `build/spine.md`: what it demonstrates,
the extension envelope a builder works inside, and where imitation stops.
The inventory is the whitelist the assembly's admission rule reads — work
no exemplar covers is simply not available to the build phase, and
builders modify or imitate only within a named exemplar's envelope
(reading is unrestricted), so declare each envelope concretely enough
that a builder can tell expected wiring from trespass. Exemplars
demonstrate patterns; a spine entry that has grown a feature set is the
phase overrunning its purpose. Read
[references/spine-authoring.md](references/spine-authoring.md) before the
first exemplar of a phase.

## Brownfield: Tear Out, Account, Rebuild

Condemnation is a duty with a trigger, not a licence to weigh: the moment
you judge a pattern unfit to imitate — you call it debt, warn a builder off
it, or find yourself writing "not the pattern" about it — and it sits in an
area the remaining work will extend, tear it out and rebuild it in this
phase. Read [references/teardown.md](references/teardown.md) when first
judging whether an area falls short, before the condemnation call — it is
what makes the judgement tractable, not a procedure for afterwards.
Warning text is never the disposition for condemned code — not a spine
boundary line, a debt-note, or an obligation queued for the assembly —
because builders imitate what they find, not what notes tell them;
replacing warning text with removed code is this phase's purpose. Apply
no cost-benefit bar at the condemnation step; the accounting below is the
cost control. This duty supersedes any standing default of filing the
smell and carrying on that a global rule supplies: in this phase, filing
is not fixing.
Condemned code *outside* the areas the remaining work extends may stand —
journal it with the reason.

There is no obligation to keep main green or tests passing during this
phase. Under a PR merge policy, run the phase on one long-lived
**structural branch** — cut from main, named `structural/<phase-start>`
so successive phases never collide — and push commits to it directly.
Open no PR for the structural work itself: the phase's product is red by
design, and a green-gated review system can never merge it, so a
structural PR deadlocks the handover. Instead, at handover, open one
**promotion PR** from the structural branch to main, `WIP:`-prefixed, and
let it stand red: its failing obligation tests are the build's standing
statement of what is still missing, kept honest by leaving them
unappeased — mask or quiet them and main can go green while capabilities
are silently absent. The promotion PR is outside the autonomous review
system's remit (structural PRs are never reviewed or merged by it): it
merges once, at the end of the assembly run, when the ledger is drained.
Opening it is this phase's job; merging it is the assembly seat's (its
landing discipline carries the conditions). Under direct-merge, work on
main directly.

Accounting is the non-negotiable half of the licence: at tear-out time —
never recalled later — every displaced capability becomes a restoration
record in `build/obligations/` or an authorised retirement in
`build/retired.md`, and before handover the applicable audit window closes
through the carried `teardown-audit` workflow (blind opposite-family
re-derivation, adjudicated against your records; fails closed), with
every discrepancy resolved.

## The Ledger and the Handover

The ledger — the header `build/ledger.md` and one record per obligation in
`build/obligations/` — is the shared control plane both build skills write:
obligations with required proofs, units as disposable tactics, status
derived from recorded evidence. Use the shared format in
[references/build-ledger.md](references/build-ledger.md); read it before
the first ledger write. This phase sets the header's phase base before any
teardown, seeds the restoration records its teardowns create, and
is the sole clearer of `needs-spine` — rewritten to `open` in the same
edit that records the covering exemplar.

Handover is artefacts, not a warm session, and five conditions make it a
handover rather than a pause: the header's handover ref is set (the
structural branch under PR mode, with the WIP promotion PR open and its
number recorded beside the ref; main under direct-merge, once the
structural work is on it); the project's declared gate — the one
command the project itself names as its gate (a Justfile recipe, a
documented command, the CI entry point), never a subset assembled here
— has run on the tree being handed over, its result recorded in the
closing journal entry: only failures owned by open obligation records
(restoration tests legitimately red) may remain — repair and re-run
for everything else — and where an owned failure halts the gate before
later stages run, run the remaining stages individually (or the gate's
run-all mode), so the handover never crosses a stage no instrument
observed; the blind spine reviews have run and
their findings are adjudicated (references/spine-authoring.md); the
closing audit is clean or recorded not-applicable; and the inventory
covers the commission's remaining work — its instruction-text
deliverables authored and marked `built-in-phase`, since the assembly
cannot build those at all — or the shortfall is marked `needs-spine` and
named in the closing journal entry. Evidence the fourth condition with a
fresh commission sweep, never the ledger alone — a ledger proves nothing
about work it never heard of: re-read the commission as it stands at
handover time and walk its remaining work against the inventory,
recording in the closing journal entry the annexe commit swept and where
each remaining piece of work is covered. Where the commission carries a
commitment register, the sweep is an ID mapping — every commitment and
goal line to the
exemplar, obligation, or already-existing capability that covers it (a
goal line resolves through its `stands on` set plus the build's closing
walkthrough of its falsifier, never through an umbrella obligation
duplicating its substrate) — and
its check runs blind: dispatch a cheap worker the bare lists — register
IDs against covered IDs, and the register's `stands on` pairs against the
mapping's resolutions — and take back only the symmetric differences,
because
a checker handed the mapping's reasoning alongside the lists rationalises
a mismatch, and an ID-only diff reads a register that kept every line and
lost its dependency structure as intact. Each unmapped commitment gets a
disposition in the same
journal entry — an obligation, a `needs-spine`, or a surfaced question —
never silence. The same sweep takes the
closing queue read (Reading the Situation): every `(structural)` item is
absorbed, struck, or named to the user — the open and blocked remainder
listed item by item — before handover. Restoration
obligations may still be open at handover, their tests legitimately red
(The Spine — function is the assembly's per-unit work). Allowance
exhausted before sufficiency is the pause case:
journal it — the queue's open and blocked structural items named as at
any boundary — leave the handover ref `unset`, resume later — the
assembly's
opening reconciliation reads an `unset` ref as a structural phase still
open.

## Grunt Work Is Never Yours

Reserve this seat for the architecture calls, the teardown judgements,
and writing the exemplars (The Spine, above). Dispatch everything
mechanical — codebase surveys, capability sweeps, renames and moves that
follow a decision already made, and digests — as workflow runs (an
impromptu inline script, or a carried workflow where one fits; the
workflow-authoring skill documents the surface), explicitly pinned to
cheap opposite-family models. The spine reviews are dispatched too, but
not cheap — route them per references/spine-authoring.md (Verification
Is Reading). A decided design does not make exemplar implementation
dispatchable.

## State and Resilience

Phase state lives in the annexe's `build/` directory, shared with the
assembly skill: `spine.md`, `ledger.md` and `obligations/`, `retired.md`,
`gaps.md`,
`teardowns/<slug>.md` (one sweep record per tear-out), `audits/` (audit
reports), `reviews/` (spine review reports), and `journal/<phase-start>.md`
(append-only narrative, never read for state; timestamps `YYYY-MM-DDTHHMM`
local throughout `build/`).
Records hold current state only, overwritten in place — git holds their
history (the annexe rule carries the discipline;
references/build-ledger.md applies it to the ledger).
Journal every event a resumption would need — an area condemned, an
exemplar landed, a discrepancy resolved — before the next stretch of work,
and commit annexe files as they change, staged by path. The phase is
resumable from the artefacts alone.

## References

- [references/build-ledger.md](references/build-ledger.md) — the shared
  ledger contract: header, the per-obligation record format, and the
  status vocabulary. Read before the first ledger write.
- [references/spine-authoring.md](references/spine-authoring.md) — judging
  spine sufficiency, the exemplar record format, and the reading-based
  verification (the blind spine reviews). Read before the first exemplar
  of a phase.
- [references/teardown.md](references/teardown.md) — the teardown
  procedure: capability sweeps and their durable records, displacement
  dispositions, and running the closing audit. Read when first judging
  whether an area falls short, before any condemnation call.
- `teardown-audit` (carried workflow) — the closing audit (blind
  re-derivation over the audit window, adjudication against the ledger,
  retired list, and sweep records).
- [references/model-selection.md](references/model-selection.md) — tier
  judgement for dispatched workers. Read before the phase's first
  dispatch; exemplar code stays at this seat regardless of its tier
  tables (The Spine).
- [references/effort-calibration.md](references/effort-calibration.md) —
  effort levels for dispatched workers. Read alongside model-selection
  when pinning a dispatch.
