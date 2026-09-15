---
name: Shakedown
description: Run a commissioned project's detection-only pre-merge QA campaign after its build ledger is discharged — drive the assembled product black-box as its first real users, judge function, experience, and output quality, package findings as build work, and close with a report and merge recommendation. Deliberately heavyweight — a campaign spans sessions and multiple quota pools — so invoke it only on the user's explicit instruction to start, resume, operate, or close a campaign. Not for per-unit build verification (assembly), spine design (maestro), ordinary code review (review-panel), or one-off browser QA (playwright); fixing what it finds is assembly's work.
---

# Shakedown

Take the assembled product on its shakedown: agents act as its first real
users, before the human ever sees it on the default branch. The campaign's
question is not "does each piece work" — the build phase proved that per
unit — but "does the whole thing hold up under real use": launched and
driven through its real interfaces as a person would, used for sustained
sessions, actively pushed until it breaks, and judged on the experience
and the quality of what it produces, not merely on whether features exist.

The campaign **detects; it never fixes the product**. Every dispositioned
defect is packaged as ordinary build work and handed upstream — fixing
here would make this a second, slower build phase, and the packaged
records keep the fix inside the build's own proof discipline. Keep the
candidate revision frozen and the tested artefact tied to it throughout
the campaign; that correspondence is checked, never assumed
(references/campaign.md, *Advancing the Campaign*). The deliverable is
the **closing report** with its merge recommendation.

One campaign serves one commission. The seat judges and dispatches:
workers drive, instruments measure, judges read evidence, while the seat
decides materiality, routing, and closure. All dispatch runs as workflows
— the Workflow Authoring skill documents the surface; read it before the
campaign's first dispatch. A singular job dispatches as a single worker
(an inline one-call script, or the delegate tool); a fan-out (charter
waves, judgement waves) runs as a workflow script when hand-issuing it
would re-decide staffing per call, and as concurrent single workers when
it is small. The carried wave scripts — `driver-wave` (one driver per
prepared black-box workspace) and `judgement-wave` (baseline sweeps and
deep passes over imported evidence), started by name — are the default
scripted shape; a campaign authors its own (saved to the topic, or
inline) only where a wave's shape genuinely departs from them. Staffing
lives in the campaign record's staffing table — per dispatch class,
carrying the per-charter variations and escalations charters.md
prescribes — which scripts and call sites read rather than re-decide.
Everything unblocked dispatches concurrently; a bound needs a named
constraint ([references/campaign.md](references/campaign.md),
*Advancing the Campaign*).

## Open: Handover, Currency, State

Read the campaign record (`build/qa/campaign.md`) **before touching the
repositories**: a record with no closing report (`build/qa/report.md`) is
a campaign to resume — verify the branch tip still matches its recorded
candidate sha before any pull, and a moved tip is a campaign-state event
([references/campaign.md](references/campaign.md), *Advancing the
Campaign*), never something to fast-forward past. Only with no open
campaign do you bring the annexe and code repo current (fetch; behind
and clean, pull fast-forward; anything else, surface it and ask). A
project with no commission, build ledger, or annexe is not this skill's
shape: say so and stop.

For a fresh campaign, check the handover: the build ledger
(`build/ledger.md` and `build/obligations/`) must show every record
`proven` or `waived`, **except** records whose Proof names a
campaign-tier rung — those are this campaign's own work, adopted at open
and discharged by its evidence, never a reason to refuse the handover.
The record format and status rules are
[references/build-ledger.md](references/build-ledger.md); read it before
reading the ledger. Anything else open is an assembly run still in
progress — tell the user, and do not start. The opening **candidate
revision** is the ledger header's handover ref at its frozen tip —
record its sha in the campaign record before the first session, with the
freeze note that assembly work on this branch waits for the close. That
ref is the **structural branch** — the build's working branch — and the
campaign's venue throughout. Read
[references/state.md](references/state.md) for the state inventory, and
resume from artefacts alone.

A fresh campaign then runs the **entry gate** before committing to the
full campaign, and derives its test map
([references/campaign.md](references/campaign.md)): charters from the
commission's commitment register and the discharged obligation ledger,
risk-tiered, each entry carrying its exercise disposition — live,
direct checks, or instrument — with the spend budget, staffing table,
and product-session width declared up front. The **coverage
ledger** (`build/qa/coverage.md`, durable across campaigns) carries
prior campaigns' evidence for surfaces the interval left untouched, and
re-queues what build work landed on — re-drives there double as
confirmation of the landed fixes. Inheriting instruments means reading
[references/instruments.md](references/instruments.md) at open too: its
staleness sweep runs alongside the coverage sweep. The map proposes; you
land it.

## The Findings Ledger Is the Authority

Everything the campaign learns lives in finding records
(`build/qa/findings/`): a finding is routed because its record says so
with evidence, standing because the user's recorded say-so is in it.
Prose status anywhere else is narrative. The campaign's remaining work
derives from two sources together — finding state, and the test map's
not-yet-exercised entries — so a fresh campaign with an empty ledger
still owes its whole map. A session that found nothing files a
triageable **null return** recording what it drove, judged, and
attempted. Read [references/findings.md](references/findings.md) before
the first dispatch — it owns the finding lifecycle end to end.

## Charters Carry the Testing

The unit of dispatch is the **chartered exploratory session**: a driver
takes one charter — a journey-shaped mission with lenses, not a script —
and uses the product for a sustained session at its real interface,
recording evidence as it goes. Drivers are dispatched **black-box**: a
workspace holding only the charter, user-facing documentation, and the
built artefact or launch recipe — never product source, unit briefs, or
build history — so what they validate is reality, not the product's
assumptions about itself. Judgement over the evidence is a separate
stage by judges from another model family: a baseline sweep that screens
every capture and transcribes at the charter's risk tier, and a deep
pass carrying the improvement mandate (better layouts, better placement,
better experience — proposed as findings for the build to apply).
[references/charters.md](references/charters.md) owns charter
composition, staffing and escalation, session conduct, evidence capture,
and the judgement stages; read it before composing the first charter.

## Instruments Make Hard Surfaces Testable

Every map entry lands with an **exercise disposition** beside its risk
tier — live, direct checks, or instrument — so a live charter is a
choice made per entry, never the shape a surface falls into. Build
reusable instruments in the annexe (`harness/`) wherever a surface
resists a person-shaped session — by **reach** (a boundary no driver
can get to or observe: pressure, timing, concurrency, injected faults,
an external service), by **depth** (one a driver reaches only slowly
or shallowly), or by **volume** (the inherited findings ledger's
mechanical re-drives). Code is cheap and testing is hard, so even
complex machinery earns its build by making a hard surface cheap and
repeatable to test. An entry about to be waived or parked for want of
a way to drive it gets its instrument proposal first; only a declined
proposal becomes a waiver. Trust an instrument only within an envelope
calibrated by re-detecting evidenced defects, never by mere agreement
with live behaviour; it never replaces the first exercise of a surface
a session can reach, an un-instrumentable channel, or the close-out
drives. Read
[references/instruments.md](references/instruments.md) before the first
instrument decision.

## Findings Route Upstream

The campaign packages; the build discharges. A dispositioned defect
converts to an ordinary **obligation record** in the build ledger,
carrying the evidence, the boundary, the regression expectation, and the
masked paths ([references/findings.md](references/findings.md),
*Packaging Findings for the Build*). The package is the whole hand-off:
assembly discharges it in its own run after the campaign closes, with
its own machinery — this campaign dispatches no fix pipeline, no direct
fix, and no worker that edits the product tree, however small the repair
looks, because "detection-only except sometimes" is how fix machinery
grows back. The one thing the campaign does repair is its own
instruments — annexe infrastructure, not the product
([references/instruments.md](references/instruments.md)).

Confirmation rides the next campaign: when packaged work lands, the
coverage ledger re-queues the touched areas, and the re-drive at the
boundary that raised each finding — instrumented where the boundary is
mechanical, live where it bears judgement — resolves its record or
reopens it.

## The Close: Report and Recommendation

The campaign closes honestly in either direction — when its map is
discharged, or earlier when continuing is not realistic: budget spent,
the remainder blocked, the user's instruction to stop, or a leakage
hand-off. An early close names what was and was not covered; it is
never a quiet extension. Every close writes the closing report
(`build/qa/report.md`), which leads with the **merge recommendation**
under the floor rule — no Critical or High finding unresolved means
merge-ready by default, and the seat deviates only with recorded
reasoning — and is delivered to the user (a page in the topic, or the
conversation), the annexe copy remaining the artefact of record. Under PR
mode, a merge-ready close also releases the promotion PR — its body
brought current first — and merges it only with fully green CI; any
other CI state leaves it de-WIPed with the merge the user's own act.
[references/campaign.md](references/campaign.md) owns the close
conditions, the floor's full statement, the merge conditions, and the
report's contents.

The campaign can also refuse: when findings cluster around one upstream
gap — a class of defect the per-unit proof should have caught — halt the
affected charters, write the leakage report, reopen the affected
obligations, and close with the branch returned to assembly
([references/campaign.md](references/campaign.md), *Leakage and the
Hand-Off*). The honest claim this phase makes is detection, not
prevention — the late tail's size was set upstream, and absorbing
build-phase remediation here silently is how a QA loop becomes a second
build phase.

## Quota Is Observed, Never Assumed

Every routing decision reads allowance state as a fact with provenance
and a reset time; a stale exhaustion is re-read once its reset passes.
A pool whose allowance cannot be read is tracked by the budget's
measured-spend discipline instead
([references/campaign.md](references/campaign.md)).
Near a ceiling, judgement legs may step down inside their family per
their own floors, but drivers never step below the driver default —
exploration under-staffed fails silently, so the affected charters
pause instead. Never silently drop a judgement leg to save quota — an
unjudged session is spend without evidence. When no family other than
the driver's has allowance, the cross-family judgement property is
unavailable: pause what needs it and tell the user.

## State and Resilience

Campaign state lives in the annexe's `build/qa/` directory
([references/state.md](references/state.md)): records hold current state
only, overwritten in place; journal events before the next sleep; commit
annexe files as they change, staged by path. The campaign resumes from
artefacts alone. Chat is a digest derived from the record, triggered by
content — a decision only the user can make, a material finding, a
charter closing, a state change (blocked, paused, leakage halt, closed) —
never by a wake firing; a wake that finds none of these produces no
message.

Hold the campaign open with a self-paced wake: set a self-wake (the
`wake_me_later` tool) before ending every turn, sized off the campaign's
state — minutes while sessions are due to land, far longer during quiet
stretches; a wake that finds nothing due spends attention for no gain, so
err long when quiet. A campaign run unattended across sessions rides a
standing schedule instead (the `schedule` tool, fresh session, gated on
the topic's open runs): each segment re-reads the campaign record and
resumes from artefacts alone. A turn ended with no wake armed is the
campaign dying silently.

## References

The sections above name each reference's read point. Two are read only
on demand beyond those:
[references/model-selection.md](references/model-selection.md) at the
first staffing decision and when overriding a pin, and
[references/effort-calibration.md](references/effort-calibration.md)
when pricing a bespoke dispatch.
