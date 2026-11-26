---
name: swage-agent-creator
description: Write, change or audit an agent this repository's product runs — its definition and its prompt — in the product's own format. Use when asked to add a runtime agent; change what one does, what starts it, what it may call, its model or its output; fix one that behaves badly; or review the product's agents. Not for a coding tool's own subagents or guidance files, the product's runtime tools or skills, or prompt wording outside an agent.
---

# Agent Creator

An agent here is one the product runs, not a coding tool's: a definition the runtime reads —
its id, the name and role it presents, its model, the tools and skills it may use, what starts
it, and what it produces and where that goes — and a prompt carrying the rest of what it knows
about its job.

The product's own code decides the format: where definitions live (code, a config file or
stored data), which fields exist and what each does, how the prompt is found, what else the
runtime puts in the agent's context, and what it validates at load. Before writing or
changing an agent, read that code, its documentation and the working agent nearest the one in
hand, and take each field's meaning from the code that reads it rather than from its name.

## The definition and the prompt

- **One unit.** A change to what starts an agent, what it may call or what it produces usually
  needs its prompt changed with it, and the reverse.
- **What it produces comes first.** What the agent hands back, and who or what consumes it,
  decides what can start it, how often it runs and what its prompt must make it do. Settle it
  before the other fields.
- **Grant what the job needs.** The agent can use what it is granted and nothing else, so a
  grant is both a capability and a risk. Every grant, skill, model and other agent the
  definition names must resolve where the agent runs.

## What the prompt carries

- **What the agent cannot get from its context.** Read how the runtime assembles the agent's
  context — what comes before and after the prompt, the tool descriptions, any state it
  injects — then write what that leaves out: the job and who uses the result, what a good
  result looks like, the hard constraints, and the decisions the agent would get wrong by
  default. Leave out what the context already gives it, such as a field the runtime shows the
  model or a tool's own description: copies drift, and the stale one wins. A field the model
  never sees is no such copy, so check which fields reach it.
- **Judgement over blanket rules.** A rule written against a bad habit is wrong wherever the
  habit would have been right. Where the right answer varies, tie the instruction to the
  situation. A rule with no exceptions belongs where the runtime enforces it — a grant the
  agent lacks, a schema on its output, a check in code — since the model may judge a prompt
  line inapplicable, and adding emphasis to a line it keeps missing does not fix it.
- **Concise and specific.** The prompt is paid for on every run, and a long one loses the lines
  that matter. Prefer concrete, checkable instructions to vague ones.
- **Current.** A stale tool name, field or label is worse than silence.
- **Goal over procedure.** State what good looks like and the constraints; reserve numbered
  steps for an operation that must be exact, such as an output a parser reads.
- **The shape its consumer reads.** Where code or another agent reads the result — a schema, a
  heading, a tool call — the prompt asks for exactly that shape, and the shape agrees with the
  definition.

## The change

Every request walks the same path; what each step costs grows with the change.

1. **Read.** An agent's prompt is a prompt, so invoke this repository's copy of the
   prompt-writing skill first — `thrum-prompt-engineering` in Thrum,
   `swage-prompt-engineering` in Swage — for an audit as much as a draft. Then read the format
   and, for a change or an audit, the agent whole: its definition, its prompt and everything
   that names it.
2. **Intake.** Sort what the request settles from what it leaves open: a new agent, a change
   or an audit; what the agent is for, in one sentence; what it produces and who consumes it;
   what starts it and where it runs; what it may call; for a change, what must stay true. Play
   the understanding back in proportion: a request that settles everything takes a line, and
   the work goes on. Ask only a genuine choice, with your recommendation; one the work cannot
   go on without waits for the answer, and any other takes the stated default, named in the
   summary. Where nobody can answer, as in a dispatched or headless run, the brief is the
   intake; stop only if what the agent is for, or what it produces, cannot be read from it.
3. **Size.** A change is **local** when it alters one field, one section of the prompt or one
   behaviour and leaves the rest untouched, and **whole** when it reshapes the agent: a new
   agent, a rewritten prompt, a change to what it produces. A wording fix changes no
   instruction.
4. **Draft.** Fill the fields from the format's contract, then write the prompt by *What the
   prompt carries*. Hold every instruction, new or touched, to three tests: a sentence a
   capable model would follow unprompted is deleted; a step, check or gate names a failure
   that has been seen and is worth its cost on every run; a stop or ask stays only where going
   on could lose someone's work or decide something that is the user's. Change a prompt by
   rewriting the affected section from its intent, never by appending a clause: accretion by
   locally correct patches is how a prompt turns procedural.
5. **Validate.** Load the agent the repository's way — its tests, the runtime's validation at
   load, a run of the product — and confirm the definition loads, its grants and references
   resolve, and the agent receives the intended prompt. Where nothing here can load it, check
   the fields against the format and say so in the summary.
6. **Review** by the change's size, as `references/review.md` sets out. A wording fix or a
   targeted correction, such as a fixed tool name, takes none.
7. **Test the behaviour** only when a named uncertainty needs an observation to settle it, or
   the user asks: a run of the agent on a realistic input it should act on, and one where it
   should hold back. A rewritten prompt and a change to what starts the agent are named
   uncertainties.
8. **Summarise** in a line what changed; then what was verified and how, each default taken,
   each review left out with its reason, each declined finding with its reason, and what stays
   uncertain.

## Auditing

An audit changes nothing unless asked. It reads each agent in scope whole, holds it to the
format and to the two sections above, and checks every claim its prompt makes — a tool name, a
field, a label, what the runtime supplies — against the code. It reports each finding with its
file and line, the problem, the evidence and what should change, the most behaviour-changing
first, and says what was read.

## References

- `references/review.md` — which reviews a change takes, running one in a fresh context, the
  defect hunt and reconciling findings; read before step 6.
- `references/concision-attack.md` and `references/preservation-blind.md` — the
  concision-attack and loss-scan prompts that `references/review.md` dispatches.
