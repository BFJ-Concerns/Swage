# Instruments

The campaign's durable test infrastructure: bespoke harnesses that make
hard-to-test things cheaply and repeatably testable. Read before the
first instrument decision.

## The Doctrine

Code is cheap and testing is hard. When a surface resists testing —
slow to drive, awkward to observe, non-deterministic, stateful, or
expensive to reach — the default is to **build the instrument** that
makes it genuinely testable, even a complex one, rather than accept
slow or shallow coverage. The build cost is paid once; the coverage
recurs whenever the surface re-queues — across campaigns as build work
lands on it, and within a campaign wherever volume is needed that live
sessions cannot carry.

An instrument is **external by construction**: it lives in the annexe
(`harness/`), outside the product repo, and drives the product only
through real boundaries — the CLI invoked, the service hit, the browser
driven. It has no access to the product's internals, so it cannot
inherit the product's assumptions — which is the property that makes
its verdicts about reality. An in-repo test that validates the
product's own mocks and expected shapes is the vacuous-test failure
this doctrine exists to displace; an instrument that imports the
product's fixtures has smuggled the same failure across the boundary.

## Lifecycle

**Commission.** An instrument is commissioned by a map entry whose
exercise disposition is `instrument` (references/campaign.md, *The
Test Map*), and three shapes of need earn that disposition:

- **Reach** — a boundary no chartered session can drive or observe:
  memory or load pressure, a timing window, concurrent access, an
  injected fault, an external service's failure modes, a state only
  reachable by construction. Here the instrument is the entry's only
  exercise, not a cheaper repeat of one, and the alternative is never
  a live session — it is a waiver.
- **Depth** — a surface a session reaches only slowly or shallowly: a
  long-running job, a rare branch, a boundary that takes many inputs to
  characterise. Live driving samples what an instrument sweeps.
- **Volume** — the inherited findings ledger: each routed finding whose
  packaged fix the interval landed owes this campaign a re-drive at a
  named boundary (references/findings.md — a routed record whose fix
  has not landed owes nothing yet). At open, alongside the staleness
  sweep, size that owed load and split it. **Mechanical** re-drives —
  the finding's Expected is fully specified and checkable without
  judgement: an exit code, an error message, a schema acceptance, a
  value at a boundary — discharge by instrument, or as direct checks
  run without building anything when the load is a handful.
  **Judgement-bearing** re-drives — experience, output quality,
  anything needing a reader's judgement — stay live, riding their
  charters; a boundary sized mechanical that resists harnessing
  reverts to live too.

Waiving an undrivable entry costs nothing now and building costs
something, so the reach case is the one a campaign loses by default.
Before parking, waiving, or downgrading an entry for want of a way to
drive it, record its **instrument proposal** on the map entry: the
boundary, what the instrument would drive and observe, the verdicts it
would return, and the build cost — then decide it. The seat
commissions a proposal the declared budget carries; one the budget
cannot carry, or whose decline would leave a commitment's falsifier
unexercised, goes to the user (the asynchronous decision venue carries
it when the user is away — references/findings.md, *Improvement
Findings*). A decline is a recorded waiver; an entry waived with no
proposal is a gap the campaign made, and the closing report says so.

Build or extend the harness only after the entry gate admits the
branch, and size the build to the load: direct checks carry a handful
of fully specified expectations, and a built instrument carries the
rest — riding mechanical checks inside exploratory charters spends the
campaign's most expensive dispatch shape on its most mechanical work,
and a high-effort build for three checks is the same disproportion
reversed. Instrument-building dispatches as its own worker on a strong
dev tier at high effort — the approach is genuinely open — with the
instrument's target surface, boundary, and intended verdicts in the
brief.

**Calibration is sensitivity, never agreement.** An instrument earns
trust only by demonstrating it *detects*: replay real evidenced defects
— the ledger's findings are the calibration library — against the
candidate that exhibited them or a deliberately broken state, and show
the instrument catches them; run known-good states and show it passes
them. Agreement with live behaviour alone proves the instrument
observes something; only caught defects prove its green is not a
vacuously always-green path. A new instrument with no applicable
evidenced defect to replay calibrates against a seeded fault — break
the surface deliberately in a scratch build, observe detection,
restore — and its calibration record marks the basis (`seeded`,
against `real`): a seeded basis is the floor, upgraded to `real` at the
first real defect the instrument re-detects, and the closing report's
instrument state reports each envelope's basis so a fleet calibrated
only against its own seeded faults reads as what it is.

**Trust is an envelope, not a property.** The calibration record
(`harness/<instrument>/calibration.md`) names what the instrument has
demonstrated it detects, on which surfaces, at which candidate — and
the envelope is exactly that, nothing wider. Inside its envelope, a
calibrated instrument replaces repeat live driving; outside it, its
verdicts are observations, not proof. Three things stay live whatever
the envelope: the first exercise of any surface a session can reach,
everything marked un-instrumentable, and the close-out drives
(references/campaign.md, *The Close*). A mechanical re-drive whose
expectation the finding record fully specifies confirms a known
boundary — it is not a first exercise; a surface beyond every session's
reach has its instrument as its only exercise, and the calibration
record says so.

**Divergence files twice.** When an instrument and a live session
disagree, open findings against both the product and the instrument;
attribution is a demonstrated step, never a default in either
direction. An instrument wrong about the product loses the affected
envelope until recalibrated.

**Staleness and maintenance.** A product surface change invalidates the
envelope covering it: the instrument re-calibrates before its verdicts
count again. The candidate is frozen for a campaign's life, so
staleness arrives between campaigns — with the build work the interval
landed — and the campaign's open handles it alongside carried coverage:
diff each envelope's calibrated surface against the tree received,
re-queue stale calibrations, and repair or retire what the intervening
build broke — inheriting a fleet that already knows how to test the
product, brought honest before it is trusted. The instruments persist
in the annexe as maintained assets, not per-run scaffolding.

**Retirement.** An instrument whose surface no longer exists is
retired — moved aside with its calibration record, not deleted; its
detection history is the evidence base a successor calibrates against.

## The Un-Instrumentable Channels

Some channels are declared permanently live, each with a judgement
rubric in the map: the feel of the UX, visual coherence, the experience
of an error message, the quality of novel product output. Automating
these produces the slow, flaky, distrusted end-to-end suite this
doctrine exists to avoid — and worse, a green one: a suite asserting
what it can assert while what mattered was never assertable. Declaring
a channel un-instrumentable is a positive act with a named rubric,
never a shrug — it is what forces the honest split between what
instruments prove and what only judgement can. Difficulty of reach
never qualifies: an unreachable boundary takes an instrument proposal
(*Lifecycle*), not a rubric.

## What Instruments Never Decide

An instrument's verdict is evidence for the findings ledger, subject
to the same triage as a session's: the seat, never the instrument,
dispositions records. An in-envelope mechanical re-drive is sufficient
evidence for the seat to resolve a routed record
(references/findings.md); a judgement-bearing surface's confirming
re-drive stays live, and the close-out drives are never replaced.
The campaign uses instruments to carry volume — the repeated, the
mechanical, the regression-shaped — so that live judgement concentrates
where only it works.
