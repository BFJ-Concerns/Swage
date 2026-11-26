---
license: Complete terms in LICENSE.txt
name: swage-skill-creator
description: Write, import, update, repair or review a skill in the format this repository's own agents load. Use when asked to create or write a skill, turn instructions or a workflow into one, fix a skill that triggers wrongly or is not followed, or bring one in from another format. Not for prompt wording outside a skill.
---

# Writing Skills

A skill is instruction text an agent loads by name when a task matches it: a short
description that decides *whether* the skill is invoked, then a body that steers the agent
once it is, plus any supporting files the body points at. This repository's own loader decides
the format: where a skill file sits, what its frontmatter or header may carry, how a skill is
matched and activated, and what limits apply. Read that loader's code or documentation, and a
working skill already in the repository, before writing or changing one; this skill gives the
craft that holds whatever the format.

## Modes

Classify the request at intake:

- **New** — nothing exists; author from the intake interview.
- **Import** — one or more existing skills, possibly written for another format, become one
  skill here. The baseline to preserve is the old skill, not no skill at all.
- **Update** — revise an existing skill, including one that misfires or is not followed. Keep
  the diagnosis separate from the first guessed fix; a remedy handed to you (a review finding,
  a user's hunch) is a candidate to re-judge, not a specification.

Import and update read `references/importing-and-updating.md` before any file changes; a
report that a skill does not trigger, over-triggers, or is not followed also reads
`references/troubleshooting.md` before diagnosing.

## Workflow

1. **Intake**, scaled to the change. For a new skill, an import, or a change to the
   description, the triggers, or what the skill verifies, settle the questions under *Intake
   Interview*, then play back the skill's job, when it triggers, what is in and out of scope,
   and what good output looks like before writing any file. For import and update, read the
   package first (step 2) so the playback carries the list of what must remain true. Ask only
   a genuine choice, each with your recommendation: one the work cannot proceed without waits
   for the answer, and one it can proceed past takes the stated default, which the summary
   names. A playback with no genuine choice in it is a report, not a question, so go on
   without waiting for a reply. A typo, link, or one-section fix needs no interview. Where no
   user can answer — a dispatched brief, a headless run — the brief is the intake: record the
   playback and any assumptions in the summary, and stop only if the skill's job, triggers,
   or good output cannot be read from it.
2. **Read before changing.** Read the loader's contract for the fields and placement in play.
   For import and update, read the whole skill — its main file and every supporting file —
   and keep an unchanged copy of it (`references/importing-and-updating.md`, *Before changing
   anything*).
3. **Draft.** Use the repository's prompt-writing skill before writing or substantially
   revising the skill's text. The body states goals, hard constraints, and success criteria,
   and enumerates steps only where an operation is fragile. Detail the agent needs at a
   specific moment goes in a supporting file behind a when-to-read link, where the format
   allows one (`references/progressive-disclosure.md`).
4. **Write the description last**, from the finished body, by *Description* below.
5. **Validate** what was produced: read its fields back against the loader's contract, then
   load it the way the repository does — its test suite, a loader check, or a run of the
   product — and confirm the skill is found with its description intact. Where nothing can
   load it here, the read-back is the check, and the summary says so.
6. **Review** at the weight the change earns — `references/review.md` (*Choosing the weight*)
   lists the rungs and their triggers; blast radius sets the minimum and the user may raise
   it, never lower it. Reconcile every finding against the text before acting on it; a
   defended survival is a valid outcome.
7. **Test the behaviour** only when a named uncertainty needs an observation to settle it or
   the user asks: a run in which the skill should be selected, and one in which it should
   not. A changed description or trigger set, and the removal of inherited scaffolding, are
   named uncertainties. Treat the loader's code as the first source of truth and runtime
   behaviour as the tie-breaker.
8. **Summarise** what changed, what was verified, each default taken in place of an answer,
   and what remains uncertain.

Jump in at whichever stage the user has already reached, but not past the intake the change
earns.

## Intake Interview

At the intake the Workflow assigns the change, establish before writing or rewriting
behaviour-bearing text:

1. What should the skill enable the agent to do?
2. When should it trigger — which user phrases, inputs, or situations count, and which
   nearby requests should *not* trigger it?
3. What does the output, decision, or side effect look like when the skill works well?
4. Which examples, input formats, integrations, tools, or dependencies matter?
5. Which edge cases, failure modes, or annoying behaviours are already known?
6. Where does it belong in the loader's layout — which scope or level, and with what grants
   or settings the format offers?
7. For import and update: is the list of what must remain true (derived from reading the
   skill) complete and correct?
8. Beyond the review rung the change earns, does the user want a selection test?

If questions 1 to 3 cannot be answered from the request, ask rather than assume.

## Principles

- **Concise for accuracy, not omission.** Shorter instructions are followed more reliably,
  but brevity is not licence to drop a gate, constraint, or distinction. Keep the main file
  lean by moving detail into supporting files, not by deleting guidance.
- **Every instruction earns its place.** The test: would the agent get this wrong without it?
  If not, cut it. A step, check, or gate also names the failure it prevents, and a change
  that touches an existing one holds it to the same test.
- **Gotchas by applicability.** A concrete environment fact that defies reasonable assumptions
  is the highest-value content a skill carries. One that bites in every use of the skill goes
  in the main file; one tied to a single mode or moment goes at the top of that moment's
  reference, with the main file's link naming the hazard.
- **Skills are composable.** Write for an agent that also has other skills, its own
  instructions, and its tools; do not restate what another skill or the runtime already does.
- **Illustrative values become defaults.** A sample filename, number, or format dropped in to
  make a sentence concrete gets copied as the canonical answer. Show a value only when it is
  a genuine requirement; otherwise describe the property.
- **Shipped files carry guidance, not commentary about it.** Strip authoring rationale,
  self-reference ("as shown above"), provenance ("added after the incident on…"), and
  present-state narration. A reader who never saw the earlier version must be served.
- **Plain Markdown, calm language.** Emphatic absolutes (ALWAYS, NEVER, CRITICAL) are for
  genuine invariants; for judgement calls, state a decision rule. Modern models enforce
  emphasis as literal hard rules and trust the calm sentences less.
- **One term per concept, and no dated facts.** Keep one name for each thing throughout the
  skill. Describe the current method rather than "before August, use the old API", and give
  one default with the one condition that selects an alternative rather than a menu.

## Description

The description is a routing key read beside every other skill's before the body loads. It
answers whether to invoke, never how the skill works.

- Name the job and the trigger conditions in terms a user would actually say. A what-only
  description leaves the *when* to inference and under-triggers.
- Third person or imperative; no "I" or "you".
- Every term present should be one the skill wants to match on. An incidental keyword — an
  output directory, a tool the skill happens to use, a list of its internal stages — matches
  as strongly as a deliberate trigger and fires the skill on requests about *those*.
- Negative clauses ("not for X — that is Y") go where a mis-route is live: fullest for a
  confusion that has happened, one short clause for a likely one (a similarly named skill,
  an adjacent stage of the same workflow), none where no confusion target exists. Where two
  skills border each other, make the clauses reciprocal.
- On update, re-derive the description whole from the current body; never append. It is not
  a changelog.
- Front-load the key use case, and keep within whatever length the loader shows or accepts.

## Body

- Lead with what shapes every use of the skill: the doctrine, the signals, the contracts
  whose violation is silent. Detail needed at one moment — a procedure, an API surface, a
  worked example — rides a supporting file read at that moment, with a link that says *when*
  to read it ("read `references/api-errors.md` if the API returns a non-200 status").
- Keep the main file short enough that the loader's budget, and the agent's attention, hold
  all of it.
- Do not duplicate between the main file and supporting files; the stale copy eventually wins.
- Where behaviour must be deterministic rather than hoped for — a forbidden action, a
  validation that must run — prefer what the runtime can enforce (a tool grant, a check in
  code) over stronger prose.
- Use concrete, realistic prompts in any test cases you write.

## Workspace

Author the skill in place, where the loader reads it. Anything transient — the unchanged copy
of a skill being imported or updated, review prompts and reports, test transcripts — goes in
a per-task directory you create with `mktemp -d` and delete when the task ends.

## Checklist

Final-state assertions:

- [ ] the intake the change earns was played back, and each genuine choice answered or its
      default named in the summary
- [ ] fields checked against the loader's contract, and the skill loaded the repository's way
- [ ] description re-derived whole: whether-to-invoke only, no incidental keywords
- [ ] the review rung stated and run; findings reconciled, never silently adopted or reversed
- [ ] import/update: whole source read first and its copy kept; nothing that must remain true
      was lost
