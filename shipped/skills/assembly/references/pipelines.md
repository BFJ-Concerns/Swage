# Pipeline Templates

The templated dispatch shapes every build unit runs through, and the seams
where your judgement enters. Read before the first dispatch of a run.

## The Template Rule

Every code-bearing unit runs through a bundled pipeline; a bare worker is
sanctioned only where the deliverable is not code (a survey, a digest) —
nothing to verify, nothing to land. A direct fix is not a unit and runs
no pipeline (references/landing.md, *Direct Fixes*). The templates exist so that
verification and landing are stages the script reaches, not decisions
anyone makes under pressure: a builder cannot land ahead of its verifier
because landing is the stage after the verdicts.

## build-verify.js — the standard unit

Stages, with the holds where the pipeline waits on you:

1. **Restate** — the builder first re-judges admission (the brief's
   `Obligation:` and `Extends:` lines exist, the named exemplar is in the
   inventory, and the work sits inside that exemplar's declared envelope —
   its **Extends at** scope without crossing **Imitation stops at** — the
   same fit your preflight settled before the brief existed, SKILL.md
   *Admission Is Binary*; a failed check comes back `admission: refused`
   for you to route `needs-spine` or fix the brief), then restates the
   brief and sketches its approach. The pipeline emits the sketch and
   **holds for your acceptance**: judge the sketch, not the paraphrase. A
   correction loops through a fresh restatement *and a fresh hold* — a
   corrected restatement you never re-read is a correction unverified. The
   accepted sketch is handed verbatim to the build stage, so what gets
   built is what you accepted. A unit killed here leaves no product code
   behind, but its refusal has already consumed a worktree, branch, and
   brief.
2. **Build** — in the unit's worktree, committing as it goes, report filed
   at the end. The stage closes with the project's check gate **green** —
   the static rung, plus the tests covering the unit's touched surfaces —
   on the tree the build finishes on, run through the project's gate
   entrypoint and recorded in the report
   (command, tree sha, result). The full test-suite gate is not the
   builder's: it runs batched over landed units at the lane gate
   (references/landing.md, *Gate Venues*; *The Lane Gate*).
   Attempts are the builder's own: it fixes
   and re-runs until green, and a builder that cannot get green reports
   that as a failure finding — what failed, what it tried — never as a
   finished record. The same route covers proof: every new test must
   discriminate — a broken product would fail it — with the fail-before
   demonstration the preferred evidence where it is cheap; a
   demonstration needing machinery out of proportion to the change is
   declared in the report with what instead evidences the
   discrimination, and a proof expectation the unit
   cannot meet honestly at its rung comes back as a failure finding —
   never as manufactured state. The verifier surfaces such declarations
   as findings, and your disposition at the triage hold answers them: a
   contract question parks the obligation, and re-runging or reshaping
   means killing the unit and re-briefing outside the pipeline — never
   re-demanding the same impossible proof. What happens once is the *record*: later stages audit
   it rather than repeat it (references/landing.md, *Gate Venues*). The
   builder never publishes: pushing, opening PRs, and merging the
   unit's work outward are the landing stage's; merging the integration
   lane into the unit branch is ordinary maintenance, open to the
   builder when the unit's work needs it.
3. **Verify** — cross-family, semantic, on the instructions in force at
   build time: the brief, the commission slice, the accepted restatement,
   and the rulings your hold decisions' notes carried (the pipeline
   passes the restatement and rulings to the verifier at the brief's
   standing, so a builder complying with an upheld dispute is not
   reported as under-delivering — write the ruling in the decision's
   `note`). A ruling that tightens something the brief left optional
   retires the brief's softener explicitly, in the ruling's own words:
   an override left implicit leaves the permissive sentence live, and
   builders resolve the tension toward it. Blind to the builder's report first
   (claims derived from the brief, then the report audited against the
   worktree): does the built thing fit the brief; does it fit the
   commission; does it stay inside the ask — overbuild is a finding —
   and does every test the unit adds or changes discriminate — could it
   fail against a broken product: the state a test
   runs over (fixture, seed, mock) and its assertions' shape are judged
   against the claim, with the discriminating proof's fail-before
   demonstrated by the verifier itself where practical (a builder's
   declared-disproportionate demonstration is judged on its substitute
   evidence and surfaced as a finding; an impractical
   demonstration of the verifier's own is declared in its
   examined-account, never skipped silently). It also takes the
   helper-shaped names the diff introduces and greps the tree for
   existing implementations — a second occurrence is a finding, pointed
   either at in-unit adoption (use the existing helper, delete the
   copy) or at a filed extraction, never grown into the unit's diff.
   The verifier audits the builder's recorded check-gate run and runs
   the scoped slice where its judgement needs a run of its own; a
   record that fails the audit is a finding, not a cue to repeat the
   gate (references/landing.md, *Gate Venues*).
   Findings come back with severity and confidence, never a verdict.
4. **Fix rounds, then the triage hold** — severe is the terminal
   severity, and the pipeline converges it without you first: while the
   verdict carries a severe finding, every finding in it goes straight
   back to the builder as an autonomous fix round — the same held
   worker, still holding what it built and why — and the delta is
   re-verified by the same held verifier, up to three autonomous rounds
   inside a five-round total budget. The loop hands over early — its
   reason in the hold content — when a fix comes back blocked or a
   re-verified severe finding survives its round: both need a
   disposition, not another round. A verdict with no severe findings
   ends the loop without spending a round. Then the pipeline emits the
   findings (with the verifier's examined-account and confidence, the
   builder report's path, and the autonomous round history) and **holds
   for your per-finding dispositions**: `fix`, `contract-question`
   (routes to `build/gaps.md`, obligation parks), or `accept-with-note`
   (journalled, with a non-empty reason in its `note`). Dispositions must
   name unique, valid finding indices and one of these three values.
   The pipeline accepts `land` when every **severe** finding is explicitly
   accepted and no disposition is `contract-question` or `fix`. A `land`
   carrying `fix` dispositions runs those repairs, re-verifies, and holds
   for a fresh decision; a contract question requires `park`. Sub-severe
   findings left undispositioned are accepted by default and return in
   the result envelope as `issues_findings` — file them to the annexe
   `ISSUES.md` when you record the landing, so neglect can accept a
   minor into the issue log but never waive a severe. A `fix` verdict
   likewise dispositions every finding it acts on by 0-based index — the
   note carries reasoning, never the record — with at least one
   disposition `fix`: an empty or fix-less disposition set re-holds (a
   fix verdict with none would dispatch an empty round), as does one
   carrying a `contract-question`, whose documented result is a parked
   obligation, never fixer input. Anything less
   re-holds, and **every hold-directed fix round is re-verified** — the
   verifier re-examines the fix delta before the next triage hold, so no
   landing rides a tree no verifier saw. Once the five-round budget is
   spent a further `fix` is refused and the hold re-arms: land or park
   on the verdict as it stands — the dramatic step is a park and a
   fresh re-briefed unit, never a sixth round. A fix round may return a
   disposition deliberately left unapplied as blocked — the sanctioned
   honest-refusal route; the re-verifier surfaces it as a finding, and
   your next dispositions answer it rather than re-demanding the same
   work.
5. **Land** — per `merge_policy` and `push_policy` (read from the annexe
   `config.toml`): direct-merge merges (a conflicted merge resolves
   in-stage, standing only on a green gate, and only a genuine semantic
   contradiction blocks — references/landing.md, *Landing Conflicts*;
   the push obeys `push_policy`); PR mode pushes the branch and opens the PR
   `WIP:`-prefixed against `integrationBranch` — a required arg in PR
   mode, and yours to supply when writing the args: the ledger header's
   handover ref (the structural branch) for every unit except two —
   one serving a `Lane: main` obligation, whose `integrationBranch` is main
   and whose worktree branch is cut from main (references/landing.md,
   *PR-Mode Topology*), and one whose work builds on a sibling's
   unmerged branch, whose `integrationBranch` is that parent branch and
   whose worktree branch is cut from its tip (references/landing.md,
   *Stack Rulings*); the scripts
   refuse to start in PR mode without it. Under `hold-for-user` the
   stage stops before the push and says so. The stage returns a **derived** result (`landed` true only on
   git/API confirmation, else what blocked it); you record the obligation
   record's evidence from that envelope — pipelines never write the
   ledger. The envelope's `outcome` is one of three: `landed` (a merge
   commit or a confirmed PR), `already-integrated` (the lane already
   contained the branch head, so there was nothing to merge or publish;
   `landed` stays true and `landing.already_integrated` is set — a
   no-op, not a failure), or `landing-blocked` (`landed` false, with
   `landing.blocked_on` naming what blocked it). A run can also end
   `verification-failed`: the verifier produced no schema-valid findings
   report across two readers, so no verdict exists and nothing landed;
   the envelope's `reason` names the labels whose turns to read before
   re-dispatching. Your account of a unit uses the word *landed* only for
   the first two. De-WIP is not a stage — it is your act, later, when the
   obligation is proven and no dependent can force a change (SKILL.md,
   Landing).

**Holds are nonce-correlated events on the run's queue.** Each hold posts
a `decision.needed` event carrying the hold's exact content
(`{stage, nonce, content, options}`) and pauses the pipeline until the
answering `decision.answer` event arrives — posted into the run from outside the
script: by you (the lead) clearing your batch of pending decisions at a
wake, or by the operator through the run surface. The decision's payload
echoes the hold's exact nonce — take it from the `decision.needed` event,
never construct it from the stage name: a stage that repeats runs as
numbered instances (`triage1`, `triage2`, …), and a decision naming an
earlier attempt's nonce is silently ignored; the script checks the nonce
itself. The payload shapes are: restate holds take
`{verdict: accept|correct|kill, nonce, note?}`;
triage holds take `{verdict: land|fix|park, nonce, note?, dispositions?:
[{finding: <index>, disposition: fix|contract-question|accept-with-note,
note?}]}` — `finding` is the 0-based position in the hold content's
findings array. Consuming an answer marks that answer delivered. After
validating its nonce, offered verdict and note type, the script acknowledges
only the corresponding request; unrelated pending events remain pending.
After posting a decision, read the run's journal before
acting on what happens next: a declined decision (a `land` the
disposition check refuses) re-holds at a fresh nonce with identical hold
content, and the reason (`land refused: …`) appears only in the run's
log entries — the next `decision.needed` event alone cannot tell a
rejected decision from the verifier re-raising the same findings. There
is no hold timeout: an unanswered decision parks that unit's run —
blocking is accepted — and every completed stage survives in the
journal, so answering late loses nothing.

Tier pins ride the template's `defaults`: builders on the build family's
flagship tier at low effort (the brief and exemplar have decided the path —
the tier carries capability, effort is the cost lever), verifiers
cross-family below their family's top tier at medium (semantic judgement is
genuinely open). Override per dispatch only with a nameable reason,
journalled.

The builder and the verifier are each one **held worker** for the unit's
life. The builder's session opens on the restatement and carries through
the build, every fix round, and — while the route's staleness reading says
it is still warm — the landing; the verifier's session opens blind to the
builder's and carries every re-verification, so the claims it derived
before reading the report are what it re-examines. A held worker's model
and effort are fixed when it opens, so fix rounds run at the build pin: the
context the worker carries into a fix round substitutes for the search a
higher effort would buy, and escalation is a park and a fresh re-brief,
never a mid-session switch. Every dispatch in the pipeline writes a
`[dispatch]` line to the run journal naming whether it **continued** a held
worker or **re-briefed** a fresh one and why — read them when a unit's
behaviour surprises you; a fix round's line carries the staleness reading
it was taken on. The choice is the script's, per stage, never the engine's:
a stage whose value is independence (the first verification, the triage
proposers, a blind sweep) re-briefs; a stage whose envelope must differ
from the worker's (a landing on another family) re-briefs; a later turn
that profits from what the worker already holds continues.

A pin is only as good as the model actually serving the worker: the run
record names the model each call resolved to, so confirm a suspect
worker's serving model there before trusting its output. When a pinned
GPT tier is genuinely unavailable, the sanctioned fallback is
`gpt-6.1-sol` at high effort, never the floor tier — a standing
operator decision (2026-07-10), and a deliberate exception to the effort
doctrine's `max` exclusion: this is not escalation on the ordinary
ladder but compensation for a forced tier drop — and the step-down is
journalled like any other declared degradation.

## estate-unit.js — end-to-end estate work

The same shape with acceptance inverted: the deliverable is tests plus
their runs, and the verify stage independently demonstrates the estate
property — each new test fails against the baseline and passes against the
proved tree; a test that cannot fail is a finding. The mechanics matter:
the fail-before check runs the new tests from the unit's worktree against
a *separate checkout* of the baseline ref (checking the baseline out in
place would delete the tests being proved) — both the builder and the
verifier do this themselves, and both runs are heavy suite
executions. The baseline ref is yours to derive when
writing the args, and it is always **the tree lacking the behaviour under
proof**: for behaviour this run built, a tree predating it — under
direct-merge the handover ref; under PR mode the handover ref *moves* as
units merge into the structural branch, so use the conditioned baseline
commit (the tree the reconciliation baseline recorded, before any unit
landed) rather than the current branch tip, and where the behaviour sits
on still-unmerged unit branches, compose the proved tree from the branch
tip plus those branches (name it in the args); for behaviour that
predates the run — a journey being retrofitted with coverage — the ledger
header's **phase base**, the last tree without it; when no reachable tree
lacks the behaviour (it predates the phase base too), the fail-before
property is honestly unavailable — the estate unit records that its tests
were proved by mutation instead (break the behaviour deliberately in a
scratch checkout, watch the test fail, restore), which is the same
property demonstrated by a different lever. A rung the environment
cannot drive at all comes back from the builder as an unavailability
report — surfaced by the verifier as its own finding for your
disposition, never satisfied by a stand-in. Estate units name their
obligation like any other; the evidence pointer is the passing run
against the tree it proved.

## docs-update.js — documentation refresh

Discharges a docs obligation (SKILL.md, *Documentation Rides the Run*):
the brief names the obligation served and the landed units covered; no
exemplar — the deliverable is documentation, not product code. The write
stage is a single worker, so brief a writer-sized slice of the landed
work; an obligation grown past that splits into several units discharged
through the run (SKILL.md, *Documentation Rides the Run*). The same
hold protocol as build-verify, with the writer and
verifier families swapped: the writer runs on the Claude family, pinned
to
`claude-opus-5-5` — an operator-settled prose pick (standing decision
2026-08-13); change it only on the user's word, never as a modernisation
sweep — and verification crosses to the GPT family, below its top tier as
verifiers always are. Landing differs from build-verify: the unit lands by
direct merge under either merge policy — into main under direct-merge,
into the integration branch under PR mode — never as a PR
(references/landing.md, *Docs Units Land Direct*).

The writer works through the project-docs skill — refresh where docs
exist, write where they are newly earned (a repository without a README
gets one) — honouring the annexe's `docs_policy` knob and, where the
project keeps parallel sets, that skill's precedence: agent pages brought
true against the code first, affected human pages re-derived from them,
indexes regenerated. The agent-guidance file is never edited
autonomously — agent-facing guidance facts compile into the unit's report
as a pending hand-off for the user. The verifier runs the accuracy audit —
agent claims and human-only claims against the
code, derived human pages against their agent counterparts — findings
with severity and confidence, never a verdict — then the triage hold and
any fix rounds run exactly as in build-verify. Changelog entries are out
of its scope: they land with their units.

## triage-wave.js — opening decomposition

Fresh runs only (references/ledger.md, The Triage Wave): cheap workers
propose decomposition criteria and obligation records over the
commission-versus-codebase delta and the annexe's `TASKS.md`
(`tasksPath` and `obligationsDir` are required inputs); the output is a
proposal document for
your judgement, never written to the ledger by the workflow. No verify
stage — proposals are checked by you at landing, against the commission.

## Running and Watching

The templates ride this skill: start one by name through the workflow-run
surface, passing the inputs its header names as the run's args (the
workflow-authoring skill teaches the surface). Each pipeline is its own
run with its own journal, event queue, and budgets; an interrupted run
resumes by replay, so nothing completed is re-executed and no salvage
ceremony is needed — the journal is the salvage.

Watch a pipeline through its run record and events: a `decision.needed`
event is a hold awaiting your answer (post the `decision.answer` event back into
the run — the hold protocol above); a run parked on an unanswered
decision stays parked until you answer, and answering wakes it. Batch
your attention: clear the pending decisions across every live pipeline at
one sitting, then leave them to cruise — the queue holds what accumulates
between your wakes. Set a self-wake (your own scheduling tools) for every
unattended stretch regardless of event delivery: a wedged worker emits no
event, so the timer bounds the silence and a silent stretch past the
bound is sampled, never slept through. For "has this run finished?", the
run's own terminal state is the signal, carried in its record and its
completion event.

Multiple units run as multiple concurrent pipeline invocations, one per
unit — admission control (disk, quota, seam collisions) happens
before each launch, not inside the scripts. At each wake, reassess the
run's width against all ready work — open obligations and
open queue items alike (references/ledger.md,
*Reconciliation*, the width step): width decays silently when dispatch
is decided one unit at a time, so an idle seat with ready work anywhere
is a state to notice, not just a wedged pipeline. When a unit leaves
active build (the width step defines the count), fill the seat at that
event: queue-sourced work fills it first while any open, unblocked
journey item no seam excludes remains (SKILL.md, *Queues
and Finds*); otherwise dispatch the next independent open obligation
— a queue judgement is a couple of short
searches and a decision, never an attention cost that
excuses an empty seat. A seat left empty anyway gets its reason
journalled (a colliding seam, resource pressure); slow reviews are
never that reason. When reporting status, separate the two axes — each unit's
position in its own sequential chain, and how many independent pipelines
are moving — so a slow review reads as one unit's wait, not the run's
pace.
