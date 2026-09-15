# The Campaign

How a campaign opens, what it tests, how it advances, and how it closes.
Read at campaign open and when judging whether to close.

## The Test Map

The map answers "what must this campaign exercise" before any session
runs. Derive it from three sources, in this order:

1. **The commission's commitment register**, verbatim — IDs, consumers,
   and falsifiers as written, never summarised: a summary is the step in
   which an internal capability's absence disappears, and the falsifiers
   tell every later judge what absence would look like. (A pre-register
   commission carries its What prose verbatim instead.)
2. **The discharged obligation ledger** — everything the build phase
   proved per unit, which this campaign exercises as journeys across
   units. The ledger names what exists; the map names how a consumer
   reaches it.
3. **The annexe's `VERIFICATION.md`** — the rungs it places at this
   phase (its campaign tier) are proof obligations of the map, alongside
   whatever proof shapes it declares for non-deterministic claims
   (`tendency`, `judged`, `accepted` rungs discharge here when the
   ladder places them here). Campaign-tier obligation records left open
   at handover are adopted into the map at open and flipped `proven` by
   campaign evidence as their entries discharge. A project with no
   ladder, or a surface the ladder never reached, is a gap: file it to
   `build/gaps.md` and park the affected entries — the commission owns
   the ladder; this campaign never writes it.

Map entries are journey-shaped — what a consumer does, not which module
answers — with a stable ID, and each names the commitments and
obligations it exercises, so coverage stays auditable in both
directions: every commitment reached by at least one charter, direct
check, or instrument, every charter naming what it exercises. The map
is a coverage view, never a script: charters constrain where testers
go, not what they must observe.

**Risk tiers are set when the map lands, and there are two.** Tier by
consequence-of-miss and novelty, not by size — and anything not clearly
low-risk is high-risk, so the ambiguous entry fails toward coverage. A
low-risk entry discharges with one session, one lens set, one oracle,
and the combined judgement pass; a high-risk entry takes the full lens
set, duplicate staffing where verdicts are hardest, a wider unflagged
sample, and both judgement stages (references/charters.md). A charter
naming several entries takes the highest tier among them. The tier
decides staffing and depth; it never decides whether a class is covered
at all.

**Every entry lands with an exercise disposition, and there are
three.** Set it beside the risk tier, before any session runs:
**live** — a chartered session, for a journey a person takes, the
first exercise of a surface a session can reach, or a channel declared
un-instrumentable with its judgement rubric named; **direct checks** —
mechanical checks the seat runs without building anything, for a
handful of fully specified expectations, recorded as a dated file under
`build/qa/checks/` that the coverage ledger cites; **instrument** — a
built harness in the annexe, for a surface that resists a session by
reach, depth, or volume (references/instruments.md, *Lifecycle*).
Recording the disposition is what makes live a decision rather than
the default a surface falls into; an entry whose disposition the seat
cannot state is not yet mapped. An entry with no way to drive it is
never simply parked: it takes an instrument proposal first
(references/instruments.md, *Lifecycle*), and the proposal, its
decision, and who made it live on the entry in the campaign record —
a proposal awaiting the user names its open venue there.

The map and the spend budget may be tightened mid-campaign; they are
never loosened. Widening the budget or narrowing the map is the user's
recorded call, listed in the closing report — deciding to test less
after the findings arrive is how campaigns rationalise themselves
shut.

## Carried Coverage

The **coverage ledger** (`build/qa/coverage.md`) is durable across
campaigns, like the instrument fleet: one entry per map entry ever
exercised — plus the queued entries packaging writes for masked paths
(references/findings.md) — naming the journey's map ID, the evidence
pointer, the sha the evidence was gathered at, and the outcome (clean,
or the findings it raised).

**The seat owns the surface set.** When a session's evidence is
imported, the seat — which, unlike the black-box driver, holds the
source — derives and records the entry's diffable surface set: exact
paths or globs, including the shared inputs that steer the journey
(configuration, schemas, manifests and lockfiles, the launch recipe).
Component or command names are labels, never diffable surfaces. An
entry whose set the seat cannot make concrete is never carried.

At each campaign's open, after the freeze check below, sweep the ledger
against the actual Git interval, including ordinary work with no obligation
package. Missing pipeline provenance never makes a changed surface untouched.
Use external verification only within its demonstrated scope and required
independence; it does not replace the campaign's first exercise or closing
drives. Sweep each entry as follows:

- An entry whose recorded surfaces are untouched between its sha and
  the incoming tree, and whose outcome was clean, is **carried** —
  listed in the map as covered by prior evidence and not re-driven.
  The diff is necessary, not sufficient: judge dependency reach too,
  and when unsure whether a change (a shared library, a toolchain or
  build-config change) reaches the journey, re-queue — carrying is an
  economy, not a right.
- An entry whose surfaces changed **re-queues**, and so does every
  entry a landed obligation package touches: sweep the obligation
  records discharged since the last campaign, re-queue their areas and
  their masked-path entries, and route any sub-severe residue their
  pipelines returned into this campaign's ledger. Where the landed
  package was a routed finding's fix, the re-drive at the boundary that
  raised it finishes that record — `resolved` on a pass, back to `open`
  on a fail (references/findings.md, *The State Machine*).
- The **spine journeys** — launch, first-run, the core loop — are
  always live, every campaign, whatever the diff says: they are the
  journey the human's own first act on the merged branch repeats.

Update the ledger as evidence lands, not at the close — a campaign that
dies mid-flight should still leave its coverage readable — and when a
session's coverage claim is later rejected, retract the entries that
session wrote: rejected coverage is not coverage.

## The Entry Gate

Before committing to the full campaign, run one **smoke expedition**: a
single black-box session across the product's spine journeys — launch,
first-run, the core loop. Its purpose is to answer one question cheaply:
is this branch ready for a campaign at all? A smoke session that cannot
launch the product, or that trips over defects the per-unit proof
plainly should have caught, refuses the branch — the leakage route
below — before the campaign's full cost is committed. A passed entry
gate is not evidence of quality; it is admission to the campaign — and
it discharges the spine journeys' live exercise for this campaign.

## Advancing the Campaign

The campaign is one swept pass over the map, not a loop: charters
dispatch in risk order, their evidence is judged, findings are triaged
and packaged, and the map's entries discharge one by one.

**Width: everything unblocked dispatches, and only a named constraint
bounds it.** Independent charters, the judgement legs over imported
evidence, probes, and instrument builds run concurrently by default — a
dispatch waits only on its own inputs. Bound a dispatch class only by a
constraint you can name, recorded in the campaign record with its
basis — and a bound earned by one class says nothing about another. In
practice one bound recurs: the **product-session width**, covering
every dispatch that runs the product — drivers, product-driving
probes, calibration replays — because they contend for the machine the
product runs on. Declare it at open as provisional — from prior
campaigns' recorded observations where they exist, otherwise a stated
estimate — and firm or drop it on this campaign's own observations:
only an observed constraint keeps a bound. Judgement legs read an
evidence tree and load almost nothing: run them as wide as the
sessions awaiting judgement, never capped by the product-session
figure and never serialised against each other — an observed limit of
the judges' own pool is, like any named constraint, a legitimate
bound.

**The budget is the real constraint, measured.** Declare the spend
budget at open per quota pool — a pool is one independently exhausting
allowance: a provider route's, or one of several a route carries — in
the unit that pool's allowance is denominated in (tokens, sessions,
requests). The default meter is the workflow engine's own per-run
usage records (each run's budget snapshot and per-route ledger); keep
spend-to-date in the campaign record by pool and by dispatch class,
with pointers to the runs it was summed from — the split that keeps
the cost asymmetry visible: drivers dominate spend, and trimming
judgement legs to save budget trims the part that costs least and
catches most. Where an allowance can be neither read nor metered, say
so in the record, name the proxy in use and its provenance, and
re-plan as measurement accumulates — a proxy counter running out is
never by itself grounds for an exhaustion close: that needs measured
spend against a read allowance, an observed quota refusal, or the
user's recorded call — a proxy-metered pool that spends its declared
budget pauses its dispatches and puts the close or an extension to the
user, neither closing on the counter nor dispatching past it. One pool's exhaustion pauses only the dispatches
that draw on it; the quota rules (SKILL.md, *Quota Is Observed, Never
Assumed*) govern stepping down and pausing. Reserve inside each
spending pool what the close needs — the close-out drives and their
judgement legs (below). A campaign that
exhausts its budget with map entries still open does not quietly extend
itself — it closes honestly, or takes the user's recorded extension.

**The freeze is checked, never assumed.** Ordinary work may continue on
separate branches, but nothing lands on the structural candidate while the
campaign is open; packaged fixes wait for the close. The campaign record
names that boundary. Keep the tested artefact tied to the frozen commit
too: an unrelated worker must not rebuild a shared binary or deployment
underneath the drivers. Verify the candidate tip and that the tested
artefact still corresponds to the recorded candidate sha at open (before
any pull), each wake sweep and before close, using the staging check in
references/charters.md. A moved tip or replaced artefact halts affected
driving and dispatch: tell the user, then close honestly on evidence still
valid for its recorded candidate sha or re-open against the new candidate
with affected coverage re-queued. Evidence is never silently re-pinned.

Escalation is evidence-driven and per charter — a rejected null return,
a coverage claim rejected under the deep pass's floor, or a charter
that stalls mid-journey re-dispatches one tier or one effort step up;
references/charters.md owns the aiming and staffing rules. The campaign
never escalates globally because one charter needed it.

## The Close

The campaign closes in one of three ways, and every close writes the
same report:

- **Coverage met** — every map entry discharged (driven, checked,
  instrumented within a calibrated envelope, or carried), no finding
  left `open`, `blocked`, or `awaiting-ruling` — every record at a
  terminal disposition for this campaign (`routed` included; routed still
  counts against the floor below) — and the close-out drives run
  (below). A remainder parked `blocked` that cannot be unparked closes
  as exhaustion instead, which names it.
- **Exhaustion** — the budget is spent (with the evidence the budget
  paragraph demands — never a proxy counter alone), or the remainder is blocked
  (paused charters, a dead mechanism, a family out of quota with no
  reset in reach), or the user says stop — their instruction is an
  exhaustion close on their authority. Close, stating exactly which
  entries were not exercised and why; "we cannot realistically test
  more now" is an honest and expected outcome, never a failure to
  hide. An exhaustion close with material findings still surfacing in
  its last sessions is itself leakage evidence — read it as the
  hand-off below, not as a quiet finish. `awaiting-ruling` records ride
  an exhaustion close as residue, their venue left live.
- **Leakage hand-off** — the campaign refused the branch (below).

**Close-out drives**, for a coverage-met close only: any map entry
discharged instrument-only in this campaign whose last *live* exercise
predates the candidate takes one live drive — an instrument's green
inside its envelope is trusted, but an entry no person-shaped session
has touched at this candidate is proof the map still owes. An entry
beyond every session's reach is exempt: the report's instrument state
lists it with its envelope's basis. The entry
gate already discharged the spine live; do not re-drive it on the same
frozen sha. These drives come from the reserved budget; if exhaustion
has consumed even the reserve, the close is an exhaustion close and
says so.

## The Closing Report

Write `build/qa/report.md`, leading with the recommendation. This
section owns the floor rule; state it nowhere else in fuller form:

- **The merge recommendation and its reasoning.** The floor: with no
  Critical or High finding unresolved (`open`, `routed`, or `blocked` —
  unattributed anomalies count at their evidence-graded severity),
  recommend **merge-ready**; Medium and below ride as recorded
  residue. With any Critical or High unresolved, recommend **hold**,
  naming the records. `resolved`, `standing`, and `downgraded` findings
  do not count against the floor; instrument findings never do. The
  seat may deviate in either direction — a hold past a clean floor
  (a material Medium cluster, spine or high-risk entries unexercised),
  or merge-ready past a High — only with the reasoning recorded here;
  an unexplained deviation is not a judgement, it is drift.
- **Coverage**: the map with each entry's evidence pointer — driven,
  checked, instrumented, or carried — and, for an exhaustion close, the
  entries
  not exercised and why.
- **Findings by the seat's materiality disposition** — material
  findings first, each with state and evidence; raw severity tallies
  are the appendix, never the headline, because judge severity grades
  observations while the report answers "should this merge". The
  appendix groups same-shaped findings; reconcile or explain grade
  differences before the close.
- **What was packaged**: the obligation records handed to the build,
  so the user can see the work the branch carries back.
- **Standing, noted, declined, and awaiting-ruling records**, each with
  its authority or its live venue.
- **Instrument state**: calibrations current with each envelope's
  basis, staleness noted, and the entries an instrument is the only
  exercise of.
- **The campaign's honest residue** — what was not exercised and why,
  and for every entry left for want of a way to drive it, the
  instrument proposal that was declined — or the admission that none
  was made.

Deliver the report to the user — a surface in the topic, or the
conversation when one is open; the annexe copy remains the artefact of
record.

Under PR mode, a **merge-ready** close also lands the promotion PR —
the ledger header's `Promotion PR` line names it — and this happens
**before the report's final write**, so the report records the outcome.
Bring the PR body current first: rewrite it to describe what the branch
now carries — the assembled build this campaign exercised, with the
campaign's outcome and recorded residue — not the structural handover
it was opened with. Remove the `WIP:` prefix, then read CI on the PR's
head by API, waiting for in-flight checks to finish and noting the head
sha the checks ran against. **Fully green, the seat merges the PR
itself** — the merge bound to that checked sha (a head that has moved
since the read means re-reading CI on the new head, never merging
past it) — a true merge commit, recorded in the report and the
journal. The merge rides the close's merge-ready judgement, recorded
deviations included: the reasoning the report carries is the merge's
authority. Anything short of fully green blocks the seat's merge —
there is no accounted-red exception here: unlike a unit merge, no
later venue re-proves what this CI run proves. In that case leave the
PR de-WIPed, name the failing or stuck checks in the report, and the
merge is the user's own act at their venue. A **hold** recommendation
leaves the prefix on and merges nothing.

The report's final write closes the campaign: a campaign record with no
`report.md` is one to resume; one with a report is finished, and the
next campaign starts fresh at open, inheriting the ledger, the coverage,
and the fleet. After the close, the promotion PR stands merged or the
branch handed back as above, and the next assembly run over the
packaged findings happens at the user's venue.

## Leakage and the Hand-Off

Leakage is the *defect-class* signal — a cluster of defects per-unit
proof should have caught (a missed check class, a systematically
untested surface, an exemplar that teaches a defect) — and it fires in
two shapes: the entry-gate refusal, before any campaign exists, and the
mid-campaign cluster. Either way the campaign is absorbing build work,
and detecting that early is this phase's honest contribution to
bounding the late tail. The hand-off:

1. Write the leakage report (`build/qa/leakage/<date>.md`): the gap,
   the evidence trail — the smoke expedition's own step record and
   captures suffice at the entry gate; mid-campaign, the findings the
   gap explains — and the affected class of work.
2. Rewrite the affected obligation records to `open`, their Evidence
   citing the leakage report (an ordinary sharpening edit under the
   shared ledger rules — status reconciled toward evidence).
3. Close the campaign: write the closing report — a leakage close
   recommends **hold**, the one recommendation its evidence supports —
   and return the branch to assembly. Assembly's own routes carry what
   goes further — structural work to maestro (`needs-spine`), contract
   questions to the commission (`build/gaps.md`). This seat routes to
   assembly only.

Mid-campaign, halt the charters the gap touches first; unaffected
charters may continue while the hand-off is written, the campaign
record saying which are halted and why. Improvement findings are never
leakage — leakage names defects upstream proof owed, and an improvement
was nobody's missed proof. A leakage hand-off is not failure theatre —
it is the difference between a QA phase and a second, slower build
phase.
