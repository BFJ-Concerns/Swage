# Troubleshooting a Skill

## The skill does not trigger

Check the description first:

- Too generic? "Helps with projects" matches nothing specific.
- Does it state *when* to use the skill, not only what it does? Models are conservative about
  optional capabilities; a what-only description under-triggers.
- Does it name the phrases users actually say, and the inputs if any?
- Is the skill even offered? Read the loader for what keeps a skill out of the agent's listing:
  a scope or level it is not loaded at, a setting that hides it, a field the loader rejects, or
  a listing budget that shortens or drops descriptions when many skills are present.

Debug technique: ask the agent in a fresh session, "When would you use the <name> skill?" The
answer mirrors the description the loader exposed. Adjust what is missing.

## The skill triggers too often

- Narrow the stated domain: "process PDF legal documents for contract review" rather than
  "process documents".
- Strip incidental keywords — an output directory, a tool the skill uses, a list of internal
  stages. Each matches as strongly as a deliberate trigger.
- Add a negative clause where a real confusion target exists, sized to the evidence: a
  mis-route that has happened earns the full "not for X — that is Y"; a likely one earns a
  short clause; none where nothing plausible competes. Make clauses reciprocal between two
  bordering skills.

## The skill loads but is not followed

Diagnose before reaching for stronger wording. Emphasis (CRITICAL, ALWAYS, bold warnings) is
usually the wrong fix: current models treat emphatic absolutes as literal hard rules and
over-apply them, while the real cause goes unaddressed.

1. **Too verbose** — agents lose focus in dense prose. Move reference material out; cut lines
   that restate what the model would do anyway.
2. **Buried** — the rule is present but mid-document, under an unrelated heading, or below a
   wall of detail. Lead with what matters under headings that name their subject; group
   related rules; do not repeat a rule at both ends of the file.
3. **Over-prescribed** — step-by-step scaffolding fights a model that plans better than the
   scaffold, and gets abandoned mid-task. State the goal, the hard constraints, and what must
   be true when done; keep steps for fragile operations only.
4. **Ambiguous** — replace a vague demand with a checkable rule: not "validate things
   properly" but "before calling create_project, verify the name is non-empty and the start
   date is not in the past; if either fails, report which and stop".
5. **Steps skipped** — enforce the step in code where the runtime allows it, rather than
   relying on prose. If it must stay in prose, state the reason the step exists; a motivated
   instruction generalises better than a bare command.
6. **Truncated** — a loader or a long session may keep only the start of a long skill. A rule
   far down a long body can vanish; move it up or shorten the body.

## The skill fails to load

Read the loader's error, then its parser: a misnamed or misplaced file, a header or
frontmatter it cannot parse, a required field missing, or a value outside what it accepts.
Unclosed quotes and a colon followed by a space in an unquoted value are the usual YAML
causes.

## Changes do not appear

Find out when the loader reads skills — at startup, per session, on each request — and whether
it reads them in place or from a copy or cache, then reload the way it expects.

## The agent feels slow or degraded

Too much always-loaded text: a long main file, many skills enabled at once inflating the
listing, or everything inline instead of behind on-demand references. Move detail out.
