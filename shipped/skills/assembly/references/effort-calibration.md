# Effort Calibration

How to choose a reasoning-effort level for a dispatched worker or agent call.
An orchestrator applies it when dispatching worker
sessions, a workflow script applies it when pinning per-stage effort, and any
session applies it to a one-off dispatch. Each model's concrete ladder,
default level, and knob live with the operator's model catalogue; this file is
the judgement that picks a position on whatever ladder is in front of you, and
the standing calibration of where the current lineups rest.

## The Principle

Effort buys search. A model's reasoning tokens are spent exploring
alternatives — which approach, which files, how to read an ambiguous
requirement, how to recover from a surprise. So the right level is priced by
the **residue**: what genuinely remains to be figured out between the dispatch
and done. When the brief, the surrounding code's own patterns, and the task's
fixed scope have already decided most of the path, there is little left to
search, and extra effort is spent re-deriving what was already decided — or,
on some models, over-persisting down open-ended tool paths. Higher is not
safer.

Price the residue, and only the residue. Three things that feel like reasons
to raise effort are not:

- **Importance.** Extra reasoning on an already-decided path addresses
  nothing about the risk that the outcome matters. Importance is met by the
  workflow the dispatch already sits in — its review gate, its landing
  checks, the model tier chosen for it. A task-specific check the brief
  names is legitimate; what importance does not buy is a generic bolt-on
  ("double-check your answer", a verify-yourself step): models that verify
  their own work unprompted just do it twice. If a genuinely load-bearing
  dispatch sits in no workflow with a gate, that is a workflow gap to fix
  where the work lands, not an effort decision.
- **Length.** Effort applies at every step of an agentic loop, so a long task
  multiplies whatever level is set. Length argues for finding the *lowest*
  level that holds quality, never for raising it.
- **Goal clarity — in either direction.** A crisp goal with an unknown path is
  still high-residue: "fix this race condition, here is the repro" is
  perfectly defined and wholly undecided. And a fuzzy goal is a brief problem,
  not an effort problem — no level compensates for a dispatch that has not
  said what done is.

## Level Anchors

Positions on a model's ladder, from its floor to its ceiling. The concrete
level names and defaults differ per model — read them from the catalogue's own
guidance and map these positions onto them.

| Residue on the path | Position | Looks like |
|---|---|---|
| None — the path is fully decided | the floor | mechanical transforms, applying a named fix, a scripted migration, run-and-report |
| Local decisions only — the brief and the surrounding patterns decide the rest | the resting level | well-briefed additive work in a codebase with established conventions |
| The approach is genuinely unsettled | one step up | debugging with an unknown cause, multi-file design inside one task, integrating with an unfamiliar surface |
| The option space is open, or the work is adversarial | near the ceiling | investigations and design surveys, hunting for why a confident claim is wrong |

The very top of a model's ladder is not one of these anchors: reserve it for
the rare task with both frontier difficulty and measured headroom over the
level below.

## The Ceiling Moves With the Deliverable

Extra search does not stay search — it becomes whatever the dispatch produces,
which is why the ceiling is not one line across all work:

- **Work that writes code tops out one step above the resting level.** The
  reasoning finds more that *could* be built and the worker builds it, so the
  excess arrives as unrequested scope in a diff — discovered at review, after
  the cost is sunk.
- **Design-only work may run a level higher.** Its excess arrives as a document
  the dispatcher reads before anything is built, where cutting it costs a
  sentence. Exploration is the deliverable rather than a route to it.

The difference is where the excess surfaces, not whether design work is harmless:
a plan the builder implements section by section carries scope into code just as
surely. So the higher level buys wider exploration of what the dispatch left
open — never licence to settle more than was asked. Over-reach in a plan is a
defect the dispatcher corrects on reading it, not a cost the level excuses.

The boundary is the deliverable, not the difficulty: a hard build unit is still
a build unit. A dispatch that designs and then implements is split in two, or
priced as a build.

## Where the Resting Level Sits

The resting level — the rung a named local decision buys, one step above the
floor — is the lowest level that holds quality for well-briefed work with real
decisions left in it, and that is **not
always the vendor's default**. Two forces pull it upward, and both are
miscalibrations:

- **Vendor defaults and headline recommendations are priced for un-briefed
  use** — a bare prompt that carries all the residue itself. A dispatched
  worker arrives with a brief that has already spent most of that residue, so
  dispatch work rests at or below the vendor's default, never above it.
- **Effort shadows tier.** An expensive model feels as if it deserves
  expensive effort. That is importance-reasoning in another coat: the tier was
  paid for *capability*, and capability substitutes for search — the stronger
  the model, the less reasoning it needs to cover the same residue. As the
  tier rises, the anchors slide *down* the ladder, not up.

Concretely, on the current lineups (positions on ladders that move — re-map
when a lineup changes):

- **Claude Fable and Opus** float at `low`, rest at `medium`; `high` is the named-residue step,
  and for code-writing work it is also the ceiling — above it these tiers
  re-derive decided things and widen the diff. Anthropic's own Opus 5
  guidance opens `xhigh` for demanding coding on eval evidence; capping
  dispatch code work at `high` is this estate's deliberate choice, not a
  disagreement about capability — dispatched workers arrive well-briefed,
  so the vendor's un-briefed headroom is already spent (see the resting-level
  note above). The design-only door opens by
  tier: an Opus design-only dispatch runs at `xhigh` per the ceiling above,
  while a Fable one tops out at `high` — the effort-shadows-tier rule in
  action: Fable, the stronger tier, sits a rung below Opus on the same
  design-only work. `max` stays out of ordinary escalation either way.
- **Claude Sonnet** floats at `low`, rests at `medium`.
- **GPT-5.5 and 5.6** float at `low`, rest at `medium`; step to `high`
  only for a named residue, with evidence preferred over instinct — higher
  effort plus open-ended tool access is this family's over-persistence failure
  mode, and OpenAI's own migration advice (keep your effort, test one level
  *lower*) points down the ladder, not up.

The floor is genuinely usable on all of these: every vendor documents its low
levels as strong on decided-path work, and a floor-shaped dispatch — recon
against a written brief, a mechanical edit, run-and-report, applying a named
fix — takes `low`, not the resting level.

## Climb From the Floor

Select the level by climbing, not by descending: start at the floor and buy
each step up with a decision you can name the worker actually making. Not
"this might need judgement" — name the decision: which files, which approach,
how to read an ambiguous requirement. If you cannot name one, the dispatch is
floor-shaped and the floor is the answer, however important or unfamiliar the
task feels.

This direction matters because the bias runs one way. In practice dispatchers
sit at a comfortable middle and only ever consider moving up; the floor
levels go unused even on work every vendor documents them as strong at —
recon against a written brief, mechanical edits, run-and-report, applying a
named fix, format conversion, collation of existing material. Those shapes
are the majority of dispatched volume, so a habit that never asks "why not
the floor?" overspends on most dispatches, not the odd one.

The climb prices each rung with the anchors above: a named *local* decision
buys the resting level; a named *open approach* buys one step more; a named
*open option space* or adversarial brief buys the near-ceiling. Record the
buying sentence wherever the dispatch is recorded — a brief, a journal entry,
a stage comment in a workflow script. A level with no recorded sentence above
the floor is a miscalibration by default, and "this is hard" or "this
matters" buys nothing: difficulty is met by tier, importance by the
workflow's gates.

## Effort and the Brief Are One Variable

Every decision the dispatch makes is a decision the worker does not reason
about. Before raising effort, ask what one more sentence in the brief would
decide — it is cheaper than reasoning tokens, and it keeps intent-shaped
decisions with the dispatcher, where intent lives. The boundary runs the other
way too: decide the intent-laden choices in the brief, leave the
intent-neutral how to the worker, and price only that remaining residue into
effort. Deciding the worker's how just to buy a cheaper level narrows a
capable model's search space and produces mechanical results.

## The Feedback Loop

Calibration is corrected by observation, in both directions: raise a level on
observed shallow reasoning against a genuinely open path; lower it on observed
over-exploration — unnecessary rework, runaway searching, settled things
re-derived. Where the dispatching context keeps routing evidence, effort
choices and their outcomes belong in it.
