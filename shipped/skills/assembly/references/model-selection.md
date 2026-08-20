# Model Selection

How to choose a model tier for a dispatched worker or agent call. An
orchestrator applies it when pinning a dispatch's model, a workflow script
applies it when pinning per-stage models, and any session applies it when
delegating a one-off task.
Each family's concrete lineup — which named models sit at its floor, its
default, and its top — lives in the operator's configured model
catalogue; this file is
the judgement that picks a position on whatever lineup is in front of you,
and, where the dispatch may choose between families, which family's lineup to
read (Family Routing below).

Tier and effort are different axes, priced separately. Effort buys search —
more exploration by a fixed capability. Tier buys the capability itself:
judgement, taste, anticipation of how words and code will be read later. A
routine task wants the cheapest tier that holds quality at resting-level
effort; an open task on a cheap tier is mis-tiered, and no effort level
compensates.

## The Principle

Price the tier by the output's downstream life, not by how hard the task feels.
Every dispatch's output has a blast radius: some is consumed once by the
dispatcher and discarded, some lands in a codebase guarded by tests and review,
and some steers other models' behaviour for months, unsupervised. A stronger
model's marginal quality is worth exactly what it saves across that whole
downstream life — so the question is never "could a cheaper tier do this?" but
"what does a defect here cost, and who catches it?".

| The output's downstream life | Position |
|---|---|
| Consumed once by the dispatcher, then discarded — recon, retrieval, run-and-report | the floor |
| Lands in the codebase, guarded by tests, review, and a compiler | the default build tier |
| Steers future sessions — skills, rules, agent prompts, commissions and intent documents | the top tier |

## Family Routing

Tier picks a position on a lineup; family picks whose lineup the position is
read from — the model id names both. When the dispatch can choose the
family — a workflow
stage, a delegated one-off, a cross-family review — route by
capability in the work's own dimension, not by standing per-task-type
assignments:

- **Doer:** the cheapest model whose capability in the work's dimension holds
  the bar — routine work runs below the dimension's best; demanding or open
  work takes it. Grunt work — recon, collection, mechanical transformation,
  fixed-shape run-and-report, anything where judgement is not the
  bottleneck — always routes to the GPT family's lineup, never to a Claude
  tier.
- **Reviewer:** the strongest model in the work's own dimension outside the
  doer's family, so the reading is cross-family. For instruction text, the
  weakest-reader rule below stands alongside: a below-drafting-tier reader
  stays in the loop besides the cross-family judge.

The concrete lineup — which models are available and at what tier — is
the operator's configured model catalogue, which is maintained as models
come and go; read the current catalogue rather than trusting memory of
it. The assignments bind only family-open dispatches: a
mechanism confined to a single family
maps positions onto that family's own lineup as before.

## Leverage Prices the Top Tier

Instruction text is drafted at the top available tier, however small the edit
looks. The arithmetic is one-sided: a skill or rule is written once and
executed hundreds of times, across months and models, so the drafting cost
amortises to nearly nothing while a subtle defect — a sentence that reads two
ways, a constraint left unstated, emphasis that over-triggers — compounds on
every future session that loads the text. Drafting also rewards precisely what
the top tier is differentially better at: anticipating misreadings, and knowing
what to leave out. And unlike code, instruction text has no compiler, no test
suite, and usually no reviewer between the draft and production — its defects
surface only as quietly drifted behaviour, long after the drafting session is
gone.

This is the deliberate counterpart to effort-calibration's rule that importance
never raises effort. Importance wants a check, because extra search on a
decided path buys nothing; leverage wants a stronger author, because the
artefact itself is what every future execution inherits.

## Review Legs Stay Below the Drafting Tier

The exception inside prompt work, and it is deliberate: legs that critique,
attack, or verify instruction text are not pinned to the top tier. The text's
production audience includes cheaper tiers and other model families, so a
reviewer below the drafting tier is a preview of the weakest reader who will
actually execute the text — its misreadings are findings about the draft, not
reviewer noise. Same-tier, same-family review also shares the author's blind
spots; a cheaper or cross-family reader catches what the author's own kind
cannot see. Keep at least one such reader in any review loop over instruction
text, and record at the dispatch site that the lower tier is chosen, not
economised — so a later maintainer doesn't "fix" it upward.

## The Floor Collects; Synthesis Starts One Tier Up

The floor tier earns its keep on collection: narrow, per-question briefs with a
fixed output shape — find this, quote the evidence, report. Handed an open
brief — "find out everything about X", "summarise this area" — it
over-summarises, and the loss is invisible because nothing downstream holds the
discarded detail. So split the work at the synthesis boundary: floor-tier
workers collect against narrow questions, and the weighing, collation, and
narrative start one tier up. A stage that cannot be split is tiered by its most
open sub-task.

## Scout Cheap, Then Spend

When an expensive dispatch would begin by reading around — surveying files,
mapping a surface, gathering candidates — put a cheap scout in front of it and
hand the expensive model the scout's findings instead. The pattern spends the
cheap tier on what it holds well (bounded retrieval) and keeps the expensive
tier's steps for the decisions only it can make. Recon before build is this
pattern; so is collection before synthesis above.

## Escalate on Evidence, Step Down Declared

Two standing permissions close the loop:

- **Escalate on the output, not the price.** When a tier's work shows shallow
  reading, a missed constraint, or mechanical results against a genuinely open
  brief, re-dispatch a tier up rather than patching downstream. No approval is
  needed to stop accepting poor output.
- **Step down declared, never silently.** Near an allowance ceiling, a fallback
  tier named at dispatch — with the step-down logged when it fires — beats
  failing while a tier is unavailable. The log line is part of the step-down: a
  constrained lineup must never silently rewrite a run's routing.

Both corrections belong wherever the dispatching context keeps its routing
evidence — the same place effort-calibration sends its observations.
