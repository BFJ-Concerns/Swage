# Findings

The findings ledger is the campaign's authority: what the campaign
knows, and what it must still do, is computed from finding state — never
from a session's tone or a campaign's vibe. The ledger is durable:
records survive the campaign that raised them, so a later campaign can
resolve what this one routed. Read this before the first dispatch.

## The Record

One file per finding, `build/qa/findings/<slug>.md`, current state only,
overwritten in place. Each carries:

- **State** — the state-machine position (below), always first.
- **Kind** — `defect` (works wrongly or not at all), `experience` (works
  but is bad to use), `output-quality` (what the product produces is
  poor), `improvement` (a concretely better state is proposed),
  `anomaly` (observed but not yet attributed), `instrument` (a harness
  is wrong — raised by dual filing or calibration failure; it takes the
  instrument branch below, never routes to the build, and never counts
  against the product's recommendation floor), `null-return` (a session
  that found nothing, filed as evidence).
- **Severity and confidence** — the judge's or driver's grading under
  the calibration below; the materiality disposition is the seat's,
  recorded separately.
- **Observed** — what was seen, at which boundary, with evidence
  pointers into the charter directory and the candidate sha the
  evidence pins to.
- **Expected** — what a user would have expected, or the oracle card
  the output was judged against.
- Whatever else the observation needs — the schema is never the filter;
  an observation that fits no field goes in anyway.

## Severity

Severity is Critical, High, Medium, or Low; confidence is graded
independently. It feeds the closing report's recommendation floor — no
Critical or High unresolved means merge-ready is the default — so
grading drift in either direction corrupts the campaign's verdict. The
calibrated line is deliberately the one the floor turns on: High versus
Medium. Anchors:

- **Severity rates what the product delivers** at the boundary the
  observation was made, never a hypothetical downstream consequence.
  A gap that could someday admit a defect rates the gap itself, not
  the imagined defect — the ceiling lifts only when the unproved
  behaviour is itself a declared commitment.
- **Wrong beats unpleasant.** Critical and High are for behaviour that
  blocks a journey, corrupts or loses what a user made, or misleads
  them into a wrong act or a wrong belief about their result —
  Critical when there is no workaround or the damage is done silently,
  High when a user could recover or would notice.
- **Misleading is demonstrated, never argued.** Presentation, wording,
  layout, signalling, and documentation findings rate the flaw itself,
  Medium at most, unless the finding's evidence demonstrates the
  misleading by one of two routes. An emitted surface — a status line,
  a success signal, an error message — escalates when the evidence
  holds both the surface and the failure it conceals or misstates: a
  label that hides a failure is High; a label that reads awkwardly is
  not. A static shipped artefact — documentation, help text, a schema
  reference — escalates when it makes a closed claim (an enumeration
  presented as complete, a stated value, a procedure, a success
  criterion) and acting on that claim as written produces a wrong
  result the evidence records. What a hypothetical consumer could be
  argued to believe or build demonstrates nothing — arguable
  consequence would scale merge exposure with documentation
  thoroughness rather than product behaviour. The calibration case: a
  shipped field absent from the documentation is Medium at most; it
  reaches High only by the closed-claim route — a total recomputed
  from an enumeration presented as complete comes out wrong.
- **Precedent holds at disposition.** Before dispositioning a graded
  finding, the seat checks it against already-graded records reducible
  to the same defect at the same kind of boundary: align the grades,
  or record the evidenced difference that justifies the split.
- **Judge against the product's own commission and declared guidance**,
  never a general posture the project has not adopted. A posture appeal
  may lower confidence; it never raises severity.
- **Latent is not delivered.** A real defect nothing in the product's
  journeys currently reaches rates what is reachable, with the latent
  path named in the record.

## The State Machine

Every record opens `open` and rests only in a state below; the seat's
disposition chooses the branch, and a resumed campaign re-derives its
work from these states alone. For the recommendation floor
(references/campaign.md, *The Closing Report*), a finding is
**unresolved** while it is `open`, `routed`, or `blocked`; every other
state is a resting disposition.

- **The routing path:** `open` → `routed <obligation-slug>` — the
  finding is packaged as build work (*Packaging Findings for the
  Build*, below). Routing ends this campaign's handling; the fix, its
  landing, and its discharge happen upstream, after the close. A
  **later** campaign finishes the path when its re-drive at the boundary
  that raised the finding — an instrument check where the boundary is
  mechanical (references/instruments.md), live where it bears
  judgement — runs at a candidate carrying the landed fix:
  a pass sets `resolved <date> <sha>` (terminal — no longer unresolved);
  a fail returns the record to `open` with the new evidence, to be
  routed again.
- **`blocked <on>`** — the routing parked it: a contract question
  routed to `build/gaps.md`, a named mechanism down. Not terminal;
  swept at every wake.
- **`noted <date>`** — the seat's cheap terminal for an observation
  judged real but **non-material and at most Medium**: recorded,
  carried in the closing report as noted residue, never silently
  dropped. A Critical or High finding, and a material finding of any
  severity, cannot be noted: it leaves `open` only by routing, by
  `standing`, or by the downgrade below — the report's visibility is
  exactly what a cheaper exit would bypass.
- **`standing <by> <date>`** — the finding stays, on the user's
  recorded say-so alone (Evidence names it; there is no self-standing).
  Terminal; listed in the closing report; does not count against the
  floor.
- **`downgraded <date>`** — judged non-material under the materiality
  rule below, with the required evidence and second judgement recorded.
  Terminal; the evidence stays in the record. Downgrade is the one
  seat-side exit for a wrongly graded Critical or High.
- **Improvements:** `open` → `routed` when contained; reshaping ones go
  `awaiting-ruling` (batched to the user) → `routed` on adoption, or
  `declined` (terminal, judgement retained) on refusal.
  `awaiting-ruling` blocks only the coverage-met close: an exhaustion
  or leakage close lists such records as residue, the ruling venue
  stays live, and they never affect the merge recommendation.
- **Anomalies:** `open` until attributed — then re-kinded to what they
  are (defect, experience, output-quality) and on to a branch above, or
  `tooling-error` (terminal) only with the reproduction evidence the
  anomaly rule demands. An unattributed anomaly is graded at the worst
  severity its evidence supports, and the floor then governs — a
  High-capable anomaly blocks merge-ready like any High.
- **Instrument findings:** `open` → `recalibrated <date>` (the harness
  repaired in the annexe and its envelope recalibrated —
  references/instruments.md) or `retired <date>` (the instrument
  withdrawn). While open, the instrument's affected envelope is not
  trusted; the product's recommendation floor is never affected.
- **Null returns:** `open` → `accepted` (coverage evidence for the map
  entries exercised; terminal) or `rejected` (insufficient depth — the
  record names the escalated re-dispatch that answers it; terminal once
  the successor session files). Depth rejection needs a named unmet
  charter commitment — journey legs not attempted, assigned oracle
  cards unexamined, breakage not attempted at stated depth — never
  marginal under-flagging, which has its own bounded rule
  (references/charters.md, *Judgement: the Deep Pass*).

Findings sharing one root area convert to a **systemic cluster**: one
root-cause package routes instead of N independent ones, every symptom
record is retained and marked routed with the cluster's slug, and each
symptom still takes its own re-drive when the coverage ledger re-queues
the area — a root-cause fix that "obviously" covers a symptom nobody
re-drove is an unconfirmed claim wearing a confident expression. Where
attribution needs evidence the sessions did not produce, the campaign
may dispatch a **diagnostic probe** — a worker that reproduces and
localises without editing the product. Contain it by construction, as
drivers are contained: it works in a disposable checkout at the frozen
candidate sha, never the structural worktree or the annexe, and only
its localisation write-up returns — a patch that comes back is
discarded, its analysis kept as evidence for the package.

## Materiality

A finding is **material** when a reasonable user would hit it in
ordinary use, would be blocked, misled, or repelled by it, or when it
indicts a commitment's falsifier. Materiality is the axis the closing
report leads with, and it is the deviation lever's main input: the
severity floor decides the default recommendation, and a *material*
Medium — one ordinary use will actually hit — is exactly what justifies
a recorded hold past a clean floor.

Raisers propose; the seat dispositions — a driver or judge grades
severity, and materiality is decided here alone. Downgrade is
structurally harder than upgrade: any raiser may propose material on
one witness's evidence; a downgrade needs recorded evidence *and* a
second judgement outside the raiser's — and the seat's own preference
for a quiet campaign is exactly the pressure the asymmetry exists to
resist. Convergence pressure defines materiality downward; the rule
makes that visible and expensive rather than silent.

## Anomalies

An anomaly is a finding until positively attributed. Classifying one as
a tooling error is a falsifiable claim: it requires reproducing the
tooling fault on a known-good input — showing the capture pipeline also
distorts a surface known to render correctly — never an assertion that it
"looks like a rendering artefact". No agent both raises and dismisses
the same anomaly.

## Improvement Findings

Improvement is part of the campaign's mandate — the product should end
up better than the campaign found it, through packaged build work — so a
concretely better state — a clearer layout, a control moved to where it
would be noticed, a friendlier flow — is a first-class finding, not
review commentary. Three exits at disposition:

- **Contained** (layout, ordering, labelling, affordance — the surface's
  intent unchanged) with a covering spine envelope: routed as build
  work like any finding, sized honestly in the package.
- **Reshaping, commitment-touching, or outside every envelope**: batched
  to the user for a taste ruling — collected, not dribbled — with the
  judge's concrete proposal and captures. The user's recorded adoption
  becomes part of the package the build receives (a commitment change
  routes through the commission as ever); agents raise design
  judgement, they do not silently redesign the product past its
  commission or its spine whitelist.
- **Declined** — by the user, or by the seat for a contained one not
  worth its cost: recorded as declined in the closing report, not
  deleted — the judgement was work, and the next campaign should not
  re-litigate it blind.

Batched rulings ride the asynchronous decision venue: put the batch to
the user as one delivered message (through the campaign's delivery
routing — the channel the user reads while away), the records at
`awaiting-ruling` naming it; sweep for answers at every wake and
resume, and apply answers as dispositions. The same venue carries
standing decisions and budget extensions when the user is away.

## Null Returns

A zero-finding session files a null return: what it drove, what it
judged (oracle cards included), what it attempted to break, with
evidence. Triage judges the return against the charter's stated depth —
a shallow return is **rejected and re-dispatched escalated**, which
makes declaring success more expensive than testing; the rejection
grounds are the state machine's (a named unmet charter commitment). An
accepted null return is coverage evidence for the map entries it
exercised; it is never evidence of quality beyond them.

## The Candidate Revision

All evidence pins to the frozen candidate sha recorded at open. Nothing
lands on that branch during the campaign; packaged fixes wait for the close.
The candidate and tested artefact must keep corresponding to that sha.
The checks and moved-candidate recovery are campaign machinery
(references/campaign.md, *Advancing the Campaign*). Work that lands
*between* campaigns is the coverage ledger's staleness input
(references/campaign.md, *Carried Coverage*).

## Packaging Findings for the Build

A dispositioned finding becomes build work by conversion, never by this
campaign editing the product:

1. **Convert the finding into an obligation record** in
   `build/obligations/` (slug `qa-<finding-slug>`; Must-be-true: the
   finding's Expected made verifiable; Proof: re-drive at the boundary
   that raised it; Source: the finding record), and set the finding
   `routed <that slug>`.
2. **Carry everything the fixer will need**: the finding's evidence
   with the boundary it was observed at, and the regression
   expectation — the test that would have caught this, where one is
   warranted.
3. **Queue the masked paths.** For each path the defect blocks from
   being reached, tested, or judged, write an unexercised entry into
   the coverage ledger naming this finding and the boundary — the
   ledger, not memory, is what makes the next campaign test behind the
   fix (references/campaign.md, *Carried Coverage*).
4. **Name the covering spine envelope** where one covers the touched
   surfaces. Where none does, say so in the record: assembly's own
   admission gate will route it (`needs-spine` to maestro, contract
   questions to `build/gaps.md`) — and a cluster of envelope-less
   defects is leakage evidence for the hand-off
   (references/campaign.md).

The package is the whole hand-off. Discharge — the pipeline, the
landing, the merge — is assembly's, in its own run after the campaign
closes; this campaign dispatches nothing against the product tree.
Sub-severe residue those later pipelines return files into the *next*
campaign's ledger at its open, alongside the landed packages' re-queued
areas.
