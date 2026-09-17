# Troubleshooting Swage Skills

Diagnostic patterns for runtime skills that are not behaving as expected. Use after confirming the skill is discovered at all (see *Discovery And Activation Failures* in `SKILL.md`).

## Skill Is Active But The Agent Does Not Follow It

**Symptom:** the skill is active, its instructions are in context, but the agent's output does not reflect the guidance.

**Check the body length and density.** Long, dense prose gets skimmed, and the whole body loads on every activation regardless of length.

- Tighten the body so the load-bearing sentences sit near the top.
- Break dense paragraphs into shorter ones; use bullets where the list reads naturally.
- If the body is long because it covers several jobs, split it into separate skills — the runtime has no on-demand supplementary-file mechanism, so a long body is paid for in full every time.

**Check for contradiction with the base instructions or another active skill.** When two prompts disagree, the agent often hedges. The skills block sits at element priority 75 in `SkillsElement` (`crates/swage-context/src/skills.rs`), below the core system prompt — so a skill cannot override base instructions by ordering. Align the skill with the base instructions, or state the override in the skill body explicitly.

**Check for ambiguous language.** "Make sure to consider X" reads as optional. "Before doing Y, confirm X" reads as a gate. Prefer gate phrasing when the guidance is non-negotiable.

## The Skill Is Never Reached

**Symptom:** the skill exists and is discovered, but it never ends up active.

Activation in Swage is user-driven or automatic — the agent does not choose to activate skills. So this splits by intended activation path:

- **Meant to be activated by the user (slash command).** The user picks from the skill list. That list carries the name, display name, level, and tools too, but the `description` is the main relevance clue — a generic or vague one gives little to recognise. State what the skill does in one short clause and name the situation it addresses. Confirm the slash name (the directory name) is what the user is typing, and that it is lowercase `[a-z0-9-]+`.
- **Meant to be always on (agent-level auto-activation).** Only skills discovered at the **agent** level are auto-activated at session start. A skill that should always be on but lives at the topic or global level will not auto-activate — move it under the agent's `skills/` directory. Confirm it is not shadowed by a same-named skill at a more specific level.

## The Skill Stays Active Longer Than Wanted

**Symptom:** a skill remains in context after it seems no longer relevant.

This is usually working as designed, not a bug — and the skill body cannot fix it, because the agent has no way to deactivate a skill.

- **Agent-level skills are active for the whole session and cannot be deactivated** (`AgentLevelCannotDeactivate`). If a skill should not always be on, it does not belong at the agent level — move it to topic or global so the user activates it deliberately.
- **User-activated skills persist per session** (stored in the SQLite `active_skills` table) until the user deactivates them via the client. If a skill should be short-lived, that is a usage expectation to set with the user, not something the body can enforce.

Do not try to solve this by writing an "unload when done" instruction into the body — there is no agent-facing skill tool for it to call.

## The Skill Lists A Tool The Agent Cannot Use

**Symptom:** a skill's `tools:` list names a tool, but the agent reports it cannot call that tool.

The `tools:` frontmatter list is **informational only** — it does not grant anything. Whether an agent can call a tool is decided entirely by that agent's `PermissionConfig` in the tool dispatch layer.

- Confirm the tool is in the `allow` list (or a granted bundle) for the agents that activate the skill. See the tool-creator skill, `references/tool-design.md`, for the permission model.
- If the tool is behind `require_approval`, the call is gated, not blocked — the agent must wait for approval. That is expected, not a skill bug.

## Frontmatter Errors

The parser (`parse_skill_frontmatter` in `crates/swage-config/src/skill.rs`) is strict about a small set of things:

- the file must start with `---` on the first line
- the frontmatter block must be closed by a standalone `---`
- `description:` is required and non-empty — without it the skill is rejected and never loaded
- `tools:` opens a list block with no inline value; each item must be `  - name` with exactly two leading spaces, and a blank line ends the list

Unknown keys are silently ignored, which means a typo in a key name (for example `descripton:` or `tool:`) looks identical to the field being missing. There is no `priority` key — if you carried one over from another runtime, the parser drops it silently. Eyeball the key spellings if the skill loads but ignores a field.

## Two Skills Contradict Each Other In Context

**Symptom:** two active skills give conflicting guidance and the agent hedges or picks inconsistently.

There is no per-skill priority to reorder them — ordering within the skills block is agent-level skills first, then user-activated skills in activation order, and that is not something you tune from frontmatter. Resolve the conflict by content:

- make the skills' scopes disjoint so they do not both speak to the same decision, or
- have one skill explicitly defer to the other for the overlapping case, or
- merge them if they are really one concern split in two.
