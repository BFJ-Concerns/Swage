---
name: skill-creator
description: Use this skill to author, update, and audit Swage runtime skills — the SKILL.md prompt fragments discovered from the Swage config hierarchy (global, topic, agent) and parsed by `crates/swage-config/src/skill.rs`, which a user activates by slash command and which the context engine folds into the agent's system context. Use when the user wants to add a new Swage skill, rework an existing one, fix discovery or activation problems, write a triggering description, or decide which hierarchy level a skill belongs at. Not for Claude Code skills under `.claude/skills/` (those use a different frontmatter schema), and not for Swage runtime *tools* (use the tool-creator skill).
metadata:
  audience: maintainers
  workflow: skill-authoring
---

# Skill Creator

Guide the session through designing, updating, and reviewing Swage runtime skills.

Swage skills are not Claude Code skills. They are focused prompt fragments discovered from the Swage config hierarchy and parsed by `crates/swage-config/src/skill.rs` (`discover_skills`, `parse_skill_frontmatter`). A user activates a skill — by slash command (`/skill-name`) or the equivalent client event — to fold its instructions into the agent's system context for the rest of the session; agent-level skills are activated automatically. Optimise for that loop. For the layer that turns activation into context, see `crates/swage-context/src/skills.rs` (`SkillsElement`).

## Skill Location And Layout

Runtime skills are discovered from a `skills/` directory at three hierarchy levels, under the Swage root (`$SWAGE_HOME`, default `~/.swage/`):

```
$SWAGE_HOME/skills/<name>/SKILL.md                                # Global
$SWAGE_HOME/topics/<topic>/skills/<name>/SKILL.md                 # Topic
$SWAGE_HOME/topics/<topic>/agents/<agent>/skills/<name>/SKILL.md  # Agent
$SWAGE_HOME/general/agents/<agent>/skills/<name>/SKILL.md         # Agent, general scope
```

(The wider Swage config cascade has a Team level too, but skill discovery — `discover_available` in `crates/swage-gateway/src/skill.rs` and the `GET /api/skills` handler — scans only these three. A `skills/` directory under a team is not loaded.)

Required rules:

- A session started without a topic runs in the **general scope**, which has no layer of its own between Global and Agent: its skills come from Global plus that agent's own `skills/` directory under `general/agents/<agent>/`.
- The directory name is the canonical identifier and must match `[a-z0-9-]+` (lowercase ASCII letters, digits, hyphens). A name with uppercase, spaces, or other characters is skipped with a warning. The frontmatter `name` field is a display name only, never the identifier.
- `SKILL.md` lives at the directory root, with a YAML frontmatter block followed by the body. A directory with no `SKILL.md` is silently skipped.
- **Override resolution:** when the same name appears at more than one level, the most specific level wins — Agent > Topic > Global. Place a skill at the narrowest level that needs it; a global skill is shadowed by any same-named skill closer to the agent.

There are no built-in example skills shipped in the repo. The integration tests under `crates/swage-gateway/tests/integration/skills_endpoint_integration.rs` and the unit tests in `crates/swage-config/src/skill.rs` are the closest thing to worked examples of the file shape.

## Core Workflow

1. Identify whether this is a new skill, an update, or a discovery/activation fix.
2. Run the intake interview (below). Do this even for updates — at minimum confirm the problem, the behaviour to preserve, and which hierarchy level the skill belongs at.
3. For an update, read the whole existing skill directory before editing, plus any agent or topic config that activates it.
4. Confirm the hierarchy level. A skill that should apply to one agent does not belong at the global level where every agent inherits it; a skill every agent needs does not belong buried under one agent.
5. If the skill lists `tools:`, confirm those tools are actually permitted to the agents that will run it — see *Tools Are Informational* below. The `tools:` list does not grant anything.
6. Draft the skill. Keep the body lean (see *Body Authoring*).
7. Summarise what changed and why before writing the live files.

Jump in at the user's actual stage, but never skip confirming the hierarchy level for a new or relocated skill.

## Intake Interview

Establish before drafting:

1. What should activating this skill make the agent better at?
2. Which agents should have it — one agent, a whole topic, or every agent globally? This sets the hierarchy level (skills are discovered at the agent, topic, and global levels only).
3. When should the user activate it? Phrase the trigger in terms of the situation or request the skill addresses, since that is what the description has to match.
4. Is this a skill that should always be on for its agents (an agent-level skill, auto-activated) or one the user reaches for situationally (activated by slash command)?
5. Does the skill rely on any tools? If so, are those tools in the relevant agents' permission config?
6. For updates: what must remain true after the change?

Run this as a back-and-forth, not a questionnaire. The point is shared understanding.

## Frontmatter

The parser (`parse_skill_frontmatter` in `crates/swage-config/src/skill.rs`) recognises exactly three keys. Unknown keys are silently ignored — do not add invented fields expecting them to do anything, and watch for typos in key names, which look identical to a missing field.

| Field | Required | Type | Purpose |
|-------|----------|------|---------|
| `description` | **required** | string | Short blurb shown in the skill list and surfaced in the activation events; it is how a user (and a model choosing from the list) recognises when a situational skill is relevant. It does **not** drive agent-level auto-activation — that is decided purely by placement at the agent level, regardless of description. A skill with no non-empty `description` is rejected and never loaded. |
| `name` | optional | string | Human-readable display name. The canonical identifier is the directory name, not this field. An empty value is ignored. |
| `tools` | optional | YAML list | Tool names the skill expects. **Informational only — see below.** |

Format rules the parser enforces:

- The file must begin with `---` on the first line; the block closes on a standalone `---`.
- `tools:` opens a list block with no inline value; each item is `  - name` (exactly two leading spaces, then `- `). An inline `tools: value` is silently ignored, and a blank line ends the list.

There is no `priority` field. Ordering in context is handled by the assembly layer, not by frontmatter — see *How Skills Enter Context*.

Example frontmatter (structural shape only — write the real description from the skill in front of you):

```yaml
---
name: Code Reviewer
description: Review changed code against the project's correctness and style standards.
tools:
  - file_read
  - file_search
---
```

## Tools Are Informational

The `tools:` list documents which tools a skill expects, and it surfaces in the `GET /api/skills` response — but **activating a skill does not grant any tool**. Tool access is governed entirely by each agent's `PermissionConfig` in the tool dispatch layer (see the tool-creator skill, `references/tool-design.md`). If a skill relies on a tool, the agents that activate it must already have that tool in their permission config; otherwise the agent reads instructions telling it to use a tool it cannot call. List the tools for documentation, then verify the permission config separately.

## Body Authoring

Keep the body short and instructional.

- Lead with the function the skill performs, in plain instructional vocabulary. Avoid persona framing ("you are the reviewer") for system-level skills.
- State principles abstractly. Do not anchor with concrete worked examples in the body — the model reproduces them. Folder names and structural conventions are exempt.
- Do not duplicate guidance already in the agent's base instructions or the project rules.
- **The whole body loads on every activation, and the runtime has no on-demand supplementary-file mechanism** — discovery reads only `SKILL.md`, and `build_active_skills` folds the entire body into context. Other files in the skill directory are not loaded. So there is no progressive disclosure to lean on: if the body is growing long, that is a signal the skill is doing too much. Tighten it, or split it into separate skills the user activates independently, rather than spilling into files the runtime will never read.

For multi-step procedures, see `references/workflows.md`. For skills that shape an output format, see `references/output-patterns.md`.

## How Skills Enter Context

Activated skills are combined by `SkillsElement` (`crates/swage-context/src/skills.rs`) into the agent's system context at element priority 75 — above retrieved memory, below the core system prompt. You do not set this per skill; it is the slot the skills block occupies as a whole.

Ordering *within* the block: agent-level skills first, then user-activated skills in activation order. There is no per-skill priority knob. If two skills could contradict each other in context, resolve it by content (see *Compose With Other Skills*), not by trying to reorder them.

## Activation, Persistence, And Deactivation

How a skill becomes active is not something the skill body controls — it is driven by the user and the runtime:

- **Slash command:** the user types `/skill-name` (optionally with trailing text that becomes the turn message). Parsed by `parse_slash_command` in `crates/swage-gateway/src/skill.rs`; names must be `[a-z0-9-]+`.
- **Client activate/deactivate events:** `ActivateSkill` / `DeactivateSkill` over the WebSocket, surfaced as `SkillActivated` / `SkillAlreadyActive` / `SkillDeactivated` server events.
- **Agent-level auto-activation:** every agent-level skill is activated automatically when a session starts (`auto_activate_agent_skills`) and **cannot be deactivated** (`AgentLevelCannotDeactivate`). Put a skill at the agent level precisely when it should always be on for that agent.
- **Persistence:** activation is per-session, stored in the SQLite `active_skills` table, and survives reconnects and gateway restarts.

The agent does not activate or deactivate skills itself — there is no agent-facing skill tool. So, unlike some runtimes, **the skill body must not instruct the agent to deactivate itself or describe an "unload" step** — it has no mechanism to do so. A user-activated skill stays active until the user deactivates it; an agent-level skill stays active for the whole session by design. Write the body for "while I am active," and let the description carry the "when to activate me" signal.

## Discovery And Activation Failures

If a skill is not appearing or not activating as expected, check in this order:

1. Directory name matches `[a-z0-9-]+`. Uppercase or spaces skip the skill with a warning.
2. `SKILL.md` is present at the directory root and starts with a `---` frontmatter block closed by a standalone `---`.
3. `description:` is present and non-empty — without it the skill is rejected.
4. The skill is at the level you expect, and is not shadowed by a same-named skill at a more specific level (Agent > Topic > Global).
5. For slash activation, the name the user types is lowercase and matches the directory name exactly.

For trigger and instruction-following problems after discovery is confirmed, see `references/troubleshooting.md`.

## Compose With Other Skills

Skills are folded into context alongside the agent's base instructions, the project rules, and any other active skills. Write each skill as if other skills may be active. Do not restate base-instruction guidance, and do not contradict other skills the agent might activate at the same time. For Swage-internal conventions, defer to the project rules in `AGENTS.md` rather than restating them inside a skill.

## Checklist

### Core Skill Checklist

- [ ] directory name matches `[a-z0-9-]+` and matches how the skill is activated
- [ ] the skill is placed at the right hierarchy level (global / topic / agent) for who should have it
- [ ] `description` is present, non-empty, and specific enough to distinguish the skill from its neighbours and trigger when it should
- [ ] the body opens with the function the skill performs, in plain instructional vocabulary, and avoids persona framing for system-level skills
- [ ] the body avoids concrete illustrative examples that would anchor future outputs
- [ ] the body does not instruct the agent to deactivate or "unload" itself — the agent has no such control
- [ ] the body is lean enough to load in full every activation; if long, it has been split into separate skills rather than into files the runtime will not read
- [ ] any `tools:` listed are confirmed present in the permission config of the agents that will run the skill (the list grants nothing)
- [ ] the skill does not duplicate base-instruction guidance or project rules

### Existing-Skill Update Checklist

- [ ] the whole current directory was read before editing
- [ ] the desired change and the behaviour that must be preserved were both named explicitly
- [ ] the hierarchy level was re-evaluated against the new scope
- [ ] frontmatter still parses cleanly under `parse_skill_frontmatter` (key spellings, `tools:` list indent)
- [ ] any rename was propagated wherever the skill is activated or referenced

## References

Read these on demand:

- `references/workflows.md` — patterns for skills whose body describes a multi-step procedure
- `references/output-patterns.md` — patterns for skills whose body shapes an output format
- `references/troubleshooting.md` — diagnosing trigger and instruction-following issues
