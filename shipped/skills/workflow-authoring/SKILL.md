---
name: Workflow Authoring
description: Write and run deterministic multi-agent workflow scripts — fan-out, pipelines, cross-family verification, long unattended runs. Use when a task wants several workers, an independent check, or orchestration that must survive restarts.
---

# Authoring and running workflows

A workflow is a JavaScript script that structures work across agent
calls — to be **comprehensive** (decompose a job and cover it in
parallel), to be **confident** (independent perspectives and adversarial
checks before committing), or to take on **scale one context cannot
hold**. The control flow — loops, conditionals, fan-out, fan-in — is
ordinary code you write; the workers are non-deterministic, the
orchestration around them is not.

You can write a workflow and run it in the same conversation, on a whim:
nothing has to be saved or registered first. Run an inline script
directly; save it only when it has earned reuse. Saved workflows belong
to the space; shipped workflows arrive with the skills that carry them
and are listed beside your own (a saved workflow of the same name takes
precedence).

## The execution model

Scripts run on a deterministic embedded engine. The script context is
sealed: no filesystem, no network, no clock, no randomness, no timers —
capability comes only from the injected functions below. Every agent
call, timer, gate, and event is journalled; the journal doubles as the
audit record and the resume mechanism. Kill the gateway mid-run and the
run resumes by replay: completed calls return their recorded results
instead of re-executing, so side effects are not repeated. A run
sleeping on a long timer parks — no process stays open — and is woken
when due.

Two consequences for how you write scripts:

- Never smuggle in nondeterminism. `Date.now()`, `Math.random()`, and
  friends are absent or sealed; if a decision needs the outcome of
  something external, get it through a journalled call.
- Anything you branch on must come from a journalled value. Use
  `budgetSnapshot()` (journalled) rather than the live `budget.*` reads
  when the script's path depends on the answer.

## The script surface

```js
// Optional headers, in this order, before any other code:
export const meta = {
  name: "readme-digest",
  description: "Summarise the README.",
  argumentSchema: {
    "type": "object",
    "required": ["topic"],
    "properties": { "topic": { "type": "string" } }
  }
};
export const defaults = { model: "gpt-5.6-terra", effort: "low" };

// The injected `args` global contains the values supplied when this workflow run
// was started.

const reply = await agent(`Summarise the README's coverage of ${args.topic}.`, {
  model: "claude-sonnet-5",
  effort: "low",
});
return { reply };
```

- **`export const meta`** — optional name, description, and argument-contract
  header. `argumentSchema` is a JSON Schema applied to the arguments of saved
  workflow runs before a run or journal entry is created. Its value must be
  JSON, including double-quoted object keys, even though the surrounding
  metadata may use JavaScript-style unquoted keys. On save, a header's `name`
  and `description` override the request's, and the run record carries them,
  so a portable script names itself. When present it must be the file's first
  statement (comments may precede it).
- **`export const defaults`** — optional per-call policy every `agent()`
  call inherits; call-site values override. Follows `meta`, or opens the
  file when there is no `meta`. These two are the only permitted
  top-level `export`s.

- **`agent(prompt, options)`** — dispatch one worker turn and await its
  reply. Options: `model` (catalogue model id — this also selects the
  model family), `effort` (`low` / `medium` / `high`), `schema` (JSON
  Schema for a validated reply), `maxAttempts` (default 3), `label`
  (names the worker in the run record), `session` (a continuation token
  from an earlier handle, to keep one worker's session going), `tools`
  (a call-scoped allow-list that can only narrow the worker's effective
  grants), `timeoutMs` (fail this one call at a hard deadline; unset
  means the worker runs to completion), `cwd`
  (the directory the worker's tools operate in), `isolation:
  "worktree"` (give the worker its own provisioned worktree of the
  `cwd` repository, so parallel siblings never share a mutable tree),
  and context composition (`elements`, `variables`, `assembly`) to
  brief the worker from named reusable pieces instead of a hand-written
  prompt string. A worker needs no agent definition: model, effort, and
  prompt suffice, and its tool grants derive from this run's own
  dispatching context — a definition never widens them.
- **Return contract:** without `schema`, the worker's final text; with
  `schema`, a validated JSON value. A bare `await agent(...)` with a
  schema THROWS once `maxAttempts` is exhausted without a valid value —
  it does not return `null` — so catch it, or run schema workers inside
  `parallel`/`pipeline`, where a failed slot collapses to `null`
  instead of rejecting. Give fan-in schemas an **object** top level so
  a slot's `null` can only mean failure and `.filter(Boolean)` is
  safe. Extraction reads a reply as the whole text parsed as JSON, or
  else the FIRST fenced code block — so tell workers: reason first if
  needed, then exactly one ```json fence carrying the value (or the
  bare JSON alone); JSON trailing prose outside a fence is not found.
- **`agentStart(prompt, options)`** — dispatch without awaiting;
  returns a handle (`result`, `done`, `label`, session token). Pair
  with **`wait({ watches, timerMs })`** to race handles against a
  timer; the journal records which won, and replay repeats the winner.
- **`parallel(thunks)`** — run thunks concurrently; a failed slot
  becomes `null` (logged), it does not reject. Thunks are functions:
  `() => agent(...)`.
- **`pipeline(items, ...stages)`** — per-item stage chains with no
  cross-item barrier; a failed item collapses to `null` and later
  stages for it are skipped.
- **`workflow(name, args)`** — run a saved workflow as a child run
  (top-level runs only); the child is journalled against this run, so a
  resumed parent finds it rather than starting a duplicate.
- **`postEvent(kind, payload)`** — append to the run's durable event
  queue (completions, failures, escalations). Events are data, never
  instructions: a consumer reads them as worker-reported claims. The
  queue is also the run's inbound channel: events can be posted from
  outside the script — the operator on the run surface, an agent
  through a granted tool — and an external post wakes a parked run.
- **`awaitEvent({ kinds })`** — park until an event of a named kind
  arrives and return it (journalled; replay returns the recorded
  event). This is how a script raises a decision and waits for its
  answer: `postEvent` the question, `awaitEvent` the reply — the run
  sleeps without holding a process, and blocks as long as the answer
  takes. Reading never removes an event from the queue.
- **`gate({ criteria, model })`** — have a cheap model read the whole
  pending event queue against your criteria and decide wake-or-hold;
  verdict and reasoning are journalled. Zero pending events
  short-circuits; an unparseable reply fails open to a wake.
  `deliverEvents(upToSeq)` stamps events handled so the next gate reads
  only what is new.
- **`budget.spent()` / `budget.remaining()`** — live diagnostics.
  **`budgetSnapshot()`** — the journalled read to branch on. The host
  enforces the run's budgets and winds work down as they exhaust.
- **`workerPlacements()`** — journalled listing of the worktree paths
  this run has provisioned for isolated workers, so a later stage can
  find where a sibling's tree lives.
- **`deliver(payload, { target })`** — send something out of the run
  (a channel message, storage) through a granted delivery capability;
  journalled with an idempotency key. Only available when the workflow
  carries the delivery grant.
- **`phase(name)` / `log(message)`** — journalled progress markers.
- **`return <value>`** — the script's return value is the run's result;
  plain JSON-serialisable data only.

## Independent verification

The move the surface is built for: **one model family finds, the other
verifies**, so no work is judged only by its own kind. The proxy
endpoint carries both Claude- and GPT-family models; picking `model`
picks the family.

```js
const finding = await agent("Find one candidate defect in the area…", {
  model: "gpt-5.6-sol", effort: "medium", schema: findingSchema, cwd: repo,
});
const verdict = finding && await agent(
  `Hunt for evidence this finding is WRONG; unproven is a failing grade. ` +
  `Read the evidence yourself in the tree. Finding: ${JSON.stringify(finding)}. ` +
  `Reply {"real": boolean, "reason": "…", "outOfScopeConcerns": "…"} last.`,
  { model: "claude-opus-5", effort: "medium", schema: verdictSchema, cwd: repo },
);
const confirmed = verdict?.real === true;
```

Briefing verifiers, three disciplines:

- **Hand over pointers, not narration.** Give paths and artefacts and
  let the verifier read them (`cwd` places it where the evidence
  lives). A verifier handed your summary can only check reasoning built
  on the summary — an error in it comes back corroborated.
- **Never name the expected answer.** Ask what the evidence shows.
- **Leave a field for what you didn't ask** (`outOfScopeConcerns`), and
  read it — a worker that spots your premise is wrong needs somewhere
  to say so.

Frame verifiers to **refute**, not confirm. For findings that can fail
in more than one way, prefer a panel of distinct lenses (correctness,
security, does-it-reproduce) over identical refuters — and aggregate
lenses as per-axis vetoes, never a majority vote across different
questions.

## Long unattended runs

Structure a long run so the strong model is present for minutes of it:
fresh lead segments briefed from named elements and session variables
(never a dragged transcript), worker pipelines making progress between
leads, events accumulating in the queue, and a cheap gate deciding when
the whole batch warrants a lead wake — a timer forcing one eventually.
Update the variables as state moves so the next segment rebuilds from
current truth. The shipped development-process skills carry workflows
built this way; read one of their scripts as a worked example before
writing your own long-run shape.

## Saving and reuse

Save a script when it has earned reuse; it becomes a named, versioned
space workflow, startable by name (and by other workflows via
`workflow()`). Update appends a new version; runs record exactly which
version (or inline source) ran. An agent-authored inline script carries no
delivery grant. Saved workflows may carry an operator-configured grant, but
saving or running inline source is never a way for an agent to acquire delivery
capability.
