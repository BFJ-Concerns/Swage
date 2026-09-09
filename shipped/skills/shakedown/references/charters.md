# Charters, Drivers, and Judgement

The chartered exploratory session is the campaign's unit of dispatch.
Read this before composing the first charter.

## Composing a Charter

A charter is a mission, not a script: where to go, what to care about,
how hard to push — with the route left to the driver. Each names the map
entries it exercises, the launch recipe, the session's expected depth,
and its lenses. Four lenses cover the campaign's questions; concentrate
duplicate staffing only where verdicts are hardest:

- **Cold start** — arrive as a new user: install or first-run, orient
  from what the product itself offers, reach first value. This is the
  journey the human's own first act on the merged branch repeats, so it
  is never skipped.
- **Sustained use** — a realistic working session of real length:
  state accumulates, features compose, yesterday's data meets today's
  session. Three messages and a screenshot is the failure mode this
  lens exists to kill; depth expectations are stated in the charter and
  enforced at triage through the null-return rule.
- **Misuse and edge pressure** — actively try to break it: malformed
  input, interrupted flows, resource pressure, wrong-order operations,
  the paths an annoyed or confused person actually takes.
- **Judgement of experience and output** — is it good, not merely
  functional: layout, discoverability, friction, error-message quality,
  and the quality of what the product produces (below).

For every map entry the charter is assigned to judge output on, it
carries an **oracle card**, written before the session: what a good
result for this exact input would contain — an exemplar, a rubric, an
independently produced reference, an invariant or comparison — and
which oracle kind applies. The card is the floor, never the script:
exploratory observations beyond the cards are the point of the session,
and outputs met along the way are judged too — but a session whose
*assigned* output judgement carries no pre-registered expectation is
rejected at triage: writing the expectation after seeing the output is
how output-quality blindness happens — nothing registers as suspect
because the output itself set the bar.

## Black-Box Dispatch

Drivers validate reality, not the product's assumptions about itself, so
the dispatch workspace is the control: it holds the charter, the
user-facing documentation, the built artefact or launch recipe, and
nothing else — no product source, no unit briefs, no build history, no
obligation records. The control works by removing breadcrumbs, not by
jailing the worker — a worker has filesystem access, so the discipline
is never to hand it a path: verify before the first session that
nothing in the workspace or the charter names the source repo, the
annexe, or any build artefact beyond the staged one, that the staged
artefact or launch recipe was built from the frozen candidate sha —
evidence pinned to a sha the artefact does not match is invalid — and
launch from the workspace, never the repo. Where the product genuinely cannot run
from a bare workspace, give the workspace the smallest launch surface
that works and record the downgrade in the campaign record — an honest
downgrade beats an over-claimed control. Diagnosis is not the driver's
job and needs no source: the driver files what it observed; the
packaged build work gets the source.

A misuse or resource-pressure charter also needs an **environment
admission** before dispatch: its target instance is the campaign's own —
disposable data, rebuildable state, no shared or live services in
reach — and the charter names the side-effect bounds. A product that
can only be driven against state the campaign does not own takes that
charter in a degraded, stated form or not at all; breakage attempts
against un-owned state are never authorised by silence.

Dispatch drivers as workflow workers — the Workflow Authoring skill
documents the surface; read it before the first dispatch — each
launched from its own black-box workspace, with a per-session timeout
sized to the charter's depth, staffed from the campaign record's
table. The carried `driver-wave` workflow is the wave's default
shape: it takes the prepared workspaces, the staffing row, and
per-charter overrides, refuses a charter with no workspace or timeout,
and places every driver leg in its own staged workspace through the
call's `cwd` — each workspace must sit inside a directory the space
has attached, since placement never widens the run's filesystem
authority. The black-box verification covers each staged workspace
before launch, and the seat sets the wave's width through the run's
`max_concurrent_agent_calls` budget (independent charters dispatch
concurrently up to the product-session width — references/campaign.md,
*Advancing the Campaign*). A
session killed at its timeout with no recorded evidence is an invalid
observation to re-run, not a null return.

## Staffing and Escalation

The driver default is the exploration family's strong mid tier at
medium effort — high effort for misuse-and-edge-pressure charters, whose
value is cunning. (Family and tier vocabulary resolves through
references/model-selection.md and the estate's routing doctrine; the
judge legs below are cross-family from whatever family the driver
resolved to.) The floor tier below the default never drives
exploration: the driver's judgement is where "this needs deeper
probing" originates, and a driver too weak to generate that signal
fails silently — the probing is not skipped visibly, it is never
conceived. The family's flagship is the per-charter exception — a
surface that defeated the default tier, or the highest-risk charters —
with the reason journalled. Text-only tiers never drive: a driver must
see the product's surfaces. The map's risk tiers cash out here: a
low-risk entry takes one session, one lens set, one oracle, and the
combined judgement pass; a high-risk entry takes the full lens set,
duplicate staffing where verdicts are hardest, a wider unflagged
sample, and both judgement stages at full scope.

Escalation is evidence-driven, per charter, and aimed: a rejected null
return, a coverage claim rejected under the deep pass's floor, or a
charter that stalls mid-journey re-dispatches one tier or one effort
step up. When the misses cluster on nameable surfaces, scope the
re-dispatch to those surfaces; otherwise the whole charter re-runs. The
campaign never escalates globally because one charter needed it.

## Session Conduct and Evidence

The driver works the charter at the product's real interface, recording
evidence as it goes into an `evidence/` directory inside its own
workspace — never the annexe, which the black-box workspace does not
contain: the step record (what it did, what it observed, what it
expected), captures of every user-visible surface it touched, and the
oracle-card comparisons for output it judged. At session end the seat
validates the evidence tree and imports it into the charter's annexe
directory (`build/qa/charters/<slug>/<session>/`, one dated session
directory per dispatch — re-drives and escalated re-dispatches sit
beside their predecessors, and imported evidence is never overwritten:
the overwrite-in-place rule governs records, and evidence a finding
cites must survive its session's later rejection), committed there
before judgement runs — judges read the imported copy. Capture discipline is
part of the work: evidence a judge cannot read is evidence that does
not exist, and the baseline sweep (below) treats an unreadable capture
as a finding in itself.

Drivers **flag** notable captures at capture time — anything that
surprised them, resisted them, or seemed worth a stronger judge's eyes —
with a line on what seemed notable. Flags prioritise deep judgement;
they never gate the baseline sweep.

An observation that fits no expected category is still filed — the
record format carries an open field precisely so the schema never
becomes the filter. A session that ends with zero findings files a
**null return**: what it drove, what it judged, what it attempted to
break, with the evidence trail. Triage may reject a null return for
insufficient depth and re-dispatch escalated
(references/findings.md, *Null Returns*).

## Judgement: the Baseline Sweep

Every session's evidence takes a baseline sweep by a judge from a
family other than the driver's, on that family's strong mid tier.
The carried `judgement-wave` workflow is the dispatch's default
shape — it takes the imported evidence directories, the judge
staffing, and each session's risk tier and declared sample size, and
runs both judgement stages concurrently for high-risk sessions or the
collapsed combined judge for low-risk ones.
Judgement dispatches as evidence imports and runs wide
(references/campaign.md, *Advancing the Campaign*): sweeps for
different sessions run concurrently, and where a high-risk charter's
two judgement stages are separate dispatches they run concurrently
too — both read the same imported tree, and neither consumes the
other's output. Two
jobs at two costs, and only the expensive one is tiered:

- **Readability screening covers every capture, every tier** — a
  capture the judge cannot read *is* a finding, because unreadable
  output is either a product defect or an evidence defect, and both
  matter. That is what catches the 2mm-tall output pane class of
  defect by construction, without anyone needing to first flag an
  anomaly, so no capture is exempt.
- **Transcription** — what a user would read from each user-visible
  capture — runs at the charter's risk tier: every capture for a
  high-risk charter or a surface's first exercise; for low-risk
  charters, every flagged capture plus the deep pass's sample.

The sweep also checks the session against its charter: depth delivered,
oracle cards used as pre-registered, captures covering the journey. On
a low-risk charter the two judgement stages collapse into one dispatch:
a single taste-tier cross-family judge runs the screen, the tiered
transcription, and the deep pass together — one worker, both stages'
duties. High-risk charters keep the stages separate.

## Judgement: the Deep Pass

On the flagged captures plus a sample of unflagged ones, a taste-tier
judge (from a family other than the driver's; taste is the column that
prices this leg) judges the experience as a demanding early user. The
sample is declared before any capture is opened — sized by the
charter's risk tier, drawn without reading contents, and widened to the
whole set when the set is small enough that sampling saves nothing:

- Is this good, not merely present — layout, hierarchy, noticeability,
  flow? The mandate is **improvement**: where a surface would be
  materially better arranged another way, say so concretely — that is
  an improvement finding (references/findings.md), and surfacing where
  the product could be better, not merely where it is broken, is part
  of what the campaign is for.
- Is the product's output actually good, judged against the session's
  oracle cards — and where output is poor, the finding names the
  observed gap, never a guessed cause: attributing poor output to the
  model, the endpoint, or the weather is the packaged fix work's
  question, and answering it at judgement is how integration defects go
  unfound.
- Does the sample of unflagged captures hold anything the driver should
  have flagged? Every miss the judge finds files as its own finding on
  its own evidence — that value is never lost — but the session's
  *coverage claim* is rejected only over a miss the judge grades
  **High or Critical** from the driver's own captures
  (references/findings.md, *Severity*). A demanding judge sampling a
  large capture set will always find *something*, so a rejection with
  no severity floor would fire on every session and discriminate
  nothing — the floor is what makes a rejection information. The judge
  records the misses and their grades; the rejection itself, and the
  escalated re-dispatch it triggers, are the seat's acts. Sub-floor
  misses are recorded in the judgement and filed as findings; the
  session's coverage claim and null return stand. Every finding
  already raised from the session survives its rejection on its own
  evidence: a discovered defect is not un-discovered by its
  discoverer's laziness elsewhere.

Judges file findings with severity and confidence, never verdicts;
disposition stays at the seat (references/findings.md).
