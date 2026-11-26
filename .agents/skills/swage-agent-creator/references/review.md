# Reviewing an Agent

A review reads the agent cold, in a fresh context apart from the authoring one, as the
runtime's model and the agent's next maintainer will. No special tooling is needed.

## Which reviews a change takes

- **None** — a wording fix, or a targeted correction such as a fixed tool name or a corrected
  field value. Validation is the whole gate.
- **Defect hunt** — every other change: a new agent, or a change to what its prompt tells it
  to do, what starts it, what it may call or what it produces. On a local change it is told
  which fields or prompt sections to read, and reads enough around them to judge fit; on a
  whole change, it reads the whole agent.
- **Concision attack** — a whole change: `references/concision-attack.md`, given the prompt's
  path and its loading profile, which is every run of the agent.
- **Loss scan** — a rewrite of an agent that works, since an author does not see what their
  own rewrite dropped: `references/preservation-blind.md`, given two directories and nothing
  else. One holds the agent as it stood before the change — its files as committed, taken
  from git, or for an agent kept as stored data, an export of its definition and prompt made
  before the first edit; the other holds the candidate in the same shape, each file at the
  same relative path in both.

The user may ask for more, never less. Name in the summary which reviews ran, and any the
change's size called for that did not, with the reason.

## Running a review

1. Write the review prompt to a file in a per-task directory made with `mktemp -d`: the brief,
   then a `## Provided Inputs` section carrying the absolute paths of the files holding the
   agent's definition and prompt, the path of the code or documentation that defines the
   format, the loading profile, and for a local change the fields or prompt sections to
   review, named by where they are. Give nothing about the authoring story — not why they
   changed, not what you hoped for. A reviewer that knows the story confirms the story.
2. Dispatch it to a fresh context that can read the files: a one-shot `claude -p` or
   `codex exec` run, or a subagent, started in the scratch directory rather than the
   repository, whose guidance would otherwise load into the reviewer. Use another model family
   where one is installed — `codex exec` for an agent drafted in Claude Code, and the reverse —
   since a same-family read rationalises away what the author missed; otherwise, a different
   model from the one that drafted.
3. Read the whole report, not its verdict line, and carry its substance into the summary.
   Delete the scratch directory when the task ends.

## The defect hunt

The brief, given to the reviewer as written:

<brief>
Review the agent at the provided paths — its definition and its prompt — as one artefact,
against the format the provided code or documentation defines. Where the inputs name fields or
sections to review, review those, reading enough around them to judge fit. Judge the text as
written;
give no credit for intent, but allege no defect without textual evidence. Prefer fewer
high-confidence findings over many weak ones. Do not spend time on praise.

Dimensions:

- **Fields** — every field is one the format accepts, holding a value it accepts, and every
  grant, skill, model and other agent the definition names resolves where the agent runs.
- **Definition and prompt agree** — both describe the same agent: what it produces, what it
  may call, what starts it.
- **Context fit** — the prompt neither restates nor contradicts what the runtime puts beside
  it, and assumes nothing the runtime does not supply.
- **Output** — what the prompt asks for is the shape its consumer reads.
- **Coherence** — the instructions can be followed and agree with each other.
- **Over-prescription** — a procedure enumerated where a goal, constraints and success
  criteria would serve a capable model better, or a blanket rule where the right answer
  varies with the situation.
- **Hygiene** — authoring rationale, provenance, narration of earlier versions, emphasis
  inflation, or notes for maintainers in text the agent reads.

Report, in this order: **Verdict** (`PASS` or `ISSUES`); **Files reviewed**; **Findings**,
each as `[F1][severity][confidence][category] path — section or lines — the problem, why it
matters, the evidence, what should change`, or `- none`; **Considered and rejected**, with a
reason each; **Summary**.
</brief>

## Reconciling findings

Check each finding against the text, the request and the code, then adopt it, adapt it or
reject it with a reason. Decline a finding that would add a step, check or gate for a failure
nobody has seen.

Judge a loss-scan finding against what the change set out to remove: a deliberate removal is
named in the summary, an unintended loss is restored, and one you are unsure of is restored
and raised with the user. When a later pass repeats an objection already reconciled, with no
new evidence, keep the earlier disposition and say so. Where a finding needs the user's
decision, put the finding itself, or a faithful summary of it, in the question.
