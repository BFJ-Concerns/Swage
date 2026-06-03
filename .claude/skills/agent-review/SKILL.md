---
name: agent-review
description: Convene a panel of review agents from a repo's `.review/` briefs — a clean-context agent per brief, splitting a large brief across several reviewers, each scoped to a subtree when its folder matches a repo directory. Use when asked to run an agent review, review the branch/PR against the project's review briefs, or audit the codebase against `.review/`. Diff mode (default) reviews branch changes and posts inline PR comments; full mode audits all in-scope code and reports to chat. Not for general bug-hunting on a diff (use code-review) or for security-only review (use security-review).
argument-hint: "[--full] [--base <ref>] [brief …]"
allowed-tools:
  - Bash(git rev-parse:*)
  - Bash(gh pr view:*)
  - Bash(python3 *plan_review.py*)
  - Bash(python3 *post_pr_comments.py*)
  - Workflow
  - Read
  - Grep
  - Glob
---

# Agent Review

Convene a panel of review agents over a repository's `.review/` briefs. Each
brief is a standing instruction for one reviewer — a concern that needs
judgement rather than a deterministic linter ("keep error copy neutral", "new
endpoints should rate-limit", "watch for accessibility regressions"). The skill
fans out clean-context agents over the briefs — blind to one another, scoped to
the relevant subtree, and a large brief split across several reviewers — then
collates their findings.

This is the rule-driven, fan-out-per-concern counterpart to `code-review`. It
does **not** hunt for arbitrary bugs — it checks the code against the repo's own
briefs. For general correctness review use `/code-review`; for security use
`/security-review`.

## The `.review/` convention

A `.review/` directory at the repo root holds one markdown **brief** per file.
A brief's position determines what it reviews:

- A brief **directly in `.review/`** applies repo-wide.
- A brief **inside a subfolder whose full nested path matches a repo
  directory** is scoped to that subtree. `.review/src/api/limits.md` reviews
  only `src/api/` when that directory exists; the match is on the full path
  (`src/api`), not just the leaf.
- A brief inside a subfolder that **matches no repo directory** still runs, but
  repo-wide, and `plan_review.py` emits a warning so authors can fix a typo or
  stale folder name. Surface that warning to the user.

A brief is plain prose: what to check and what good looks like. The agent reads
the full file, so write it for a reviewer, not a parser. A brief may carry
optional frontmatter declaring its **extent**:

```yaml
---
extent: full   # or omit for the default, diff
---
```

Scope (which subtree) and extent (how much of it) are independent axes:

- **`extent: diff`** (default, or no frontmatter) — review only what the branch
  changed within the brief's scope. Most briefs are this: "is this changed code
  good?"
- **`extent: full`** — audit the brief's whole scope on every run, regardless of
  the diff. Two kinds of concern need this: per-file judgement that must cover
  the whole tree rather than just the diff (test quality, per-file conventions),
  and whole-tree invariants (global properties, cross-file consistency, "every X
  is registered in Y"). Heavier, so reserve it for concerns that genuinely need
  it.

A full-extent brief also declares, in the same frontmatter, **how it sweeps** —
whether its files can be reviewed independently:

- **`sweep: per-file`** (default, or no frontmatter) — each file is judged on its
  own. When the scope is large, the brief is split across several reviewers, each
  handed a slice, so no single agent is swamped.
- **`sweep: whole-tree`** — the concern is a cross-file invariant that needs the
  whole scope in one view, so the brief is **never** split; it is always one
  reviewer. Reserve it for genuine whole-tree concerns, and prefer scoping it to a
  subtree so that the one reviewer's load stays reasonable.

`/review-brief` decides and sets a brief's extent and sweep when authoring; this
skill reads them.

## Runs

| Invocation | What runs | Output |
|------------|-----------|--------|
| `/agent-review` | Every brief, each at its declared extent, vs the branch | PR review with inline comments + chat recap; chat-only if no PR |
| `/agent-review <brief>…` | Only the named brief(s), at their declared extent | as above |
| `/agent-review --full [<brief>…]` | The named briefs (or all), forced to full extent | Chat summary grouped by brief |
| `/agent-review --base <ref> [<brief>…]` | Diff against `<ref>` instead of the auto-detected base | as the default run |

A default run honours each brief's extent: diff-extent briefs see only changed
files; full-extent briefs audit their whole scope and classify each finding so
change-caused ones still reach the PR (see Report). **`--full`** overrides every
selected brief to a whole-scope audit and reports to chat — use it to baseline a
new brief against existing code (`/agent-review --full <new-brief>`) without
dragging every other brief through a sweep. **Brief selection** — naming briefs —
restricts any run to those briefs.

**Base resolution** (diff mode only) decides what the branch is compared against,
most explicit first: a PR's base when there's a PR, otherwise an auto-detected
default branch (`origin/HEAD`, then `main`/`master`). When neither is right — a
branch that forks off another branch, or a repo with an unusual default — pass
**`--base <ref>`** to name what to diff against. This is the only supported
non-PR shape: agent-review reviews a PR or a branch against its base, nothing
else. A *guessed* base that turns up no changes is reported as provisional rather
than as a pass, because it as often means the wrong base as a clean branch.

A full-scope audit covers every in-scope tracked file. A **per-file** brief is
split across several reviewers when its scope is large, so the load stays bounded
however big the tree. A **whole-tree** brief can't be split — it is always one
reviewer — so that is the one to keep scoped to a subtree rather than running it
repo-wide over a large tree.

## Workflow

The fan-out runs as a Workflow so that the dispatch (one agent per shard, every
brief), the slice of files each reviewer covers, and the findings shape are fixed
by code rather than left to the model. The reviewers' judgement stays
non-deterministic — that is the point — but what they are pointed at, and the
structure they return, are not. Invoking the bundled workflow from this skill is
the user's opt-in to multi-agent orchestration — do not ask again before running
it.

The plan decides the fan-out; run the bundled workflow as written. A brief with a
large file set is **deliberately split across several reviewers by the plan** —
that is the workflow doing what it is for, iterating a large group across agents,
and it is fixed in code (`plan_review.py` sizes each brief and shards it). What is
**not** allowed is improvising on top of that at runtime: do not rewrite the
script, re-derive or re-balance the shards by hand, or add a verification pass
that re-checks the first wave. The groundedness guard lives inside each reviewer
(it must quote the code it flags; see step 3), not in a second wave of agents. If
something genuinely blocks a faithful run — the plan won't pass cleanly, or a
**whole-tree** brief's scope is too large for its single reviewer to cover well —
stop and report that to the user (for the whole-tree case, the fix is usually to
scope the brief to a subtree) rather than engineering around it silently.

1. **Plan.** Run the discovery script from the target repo, translating the
   slash arguments: `--full` → `--full`, `--base <ref>` → `--base <ref>`, and any
   bare words → brief names to restrict to.

   ```bash
   python3 ~/.claude/skills/agent-review/scripts/plan_review.py [--full] [--base <ref>] [brief-name …]
   ```

   Each brief in the plan carries its effective `extent` (`diff` or `full`) and
   `sweep` (`per-file` or `whole-tree`) alongside its `scope`, the size of its
   file set as `file_set_size`, and a `shards` list — one entry per reviewer. A
   small brief, or any whole-tree brief, has a single shard; a large per-file
   brief has several, each describing a slice of the scope. An unsharded diff
   brief's shard carries the explicit changed `files`; every other shard carries a
   1-based line range the reviewer turns into its slice by enumerating the scope
   itself, so no large file list is ever relayed through `args` (see the Workflow
   note).

   It returns JSON: the briefs, each one's resolved `scope` (or `null` for
   repo-wide), `extent`, `sweep`, `file_set_size`, `shards`, `skip_reason`, the
   detected `pr`, the `base_ref`, the `base_source`, `warnings`, and
   `template_path` (the shared reviewer method file every reviewer reads). If it
   returns an `error` (no git repo, no `.review/`, no briefs, a missing reviewer
   method file, or an unresolvable `--base`), report it and stop. Surface any
   `warnings` to the user — they usually mean a brief is mis-scoped.

   Tell the user what was diffed: the `base_ref` and how it was chosen
   (`base_source` is `pr`, `explicit`, or `default`). When it's `default` the
   base was *guessed* — say so, and if the run found little or nothing, treat
   that as possibly the wrong base and suggest `--base <ref>` rather than
   reporting a clean pass. When a brief's `shards` list has more than one entry,
   mention the split (the workflow logs it too) so the user sees a large sweep was
   spread across reviewers. A **whole-tree** brief with a large `file_set_size`
   can't be split, so flag that one as a candidate for subtree-scoping rather than
   letting a single reviewer skim hundreds of files.

2. **Fan out via Workflow.** Invoke the bundled workflow, passing the plan from
   step 1 as the only argument:

   ```
   Workflow({
     scriptPath: "~/.claude/skills/agent-review/scripts/review_workflow.js",
     args: { plan: <plan JSON from step 1> }
   })
   ```

   Do **not** pass the reviewer prompt. It is static (`prompts/reviewer-method.md`),
   the plan carries its path (`template_path`), and each reviewer reads it
   itself — relaying that multi-kilobyte, backtick-heavy prose through `args` was
   what caused the earlier "JSON Parse error" crashes (a single garbled
   character killed the run at `JSON.parse`, 0 agents). Keep `args` to just the
   compact plan.

   The workflow launches one schema-validated agent per shard, in parallel: one
   agent for a small or whole-tree brief, several for a large per-file brief, each
   covering a slice of the scope. Each agent reads the shared reviewer method and
   its own brief file, and reviews only its assigned files — reviewers stay blind
   to one another. A **diff-extent** brief inspects the branch diff for its changed
   files and flags what the change introduces or touches; a **full-extent** brief
   enumerates its scope (with `git ls-files`) and audits it, classifying each
   finding as change-caused (`preexisting: false`) or independent
   (`preexisting: true`). A `--full` run forces every brief to a plain whole-scope
   audit instead. It returns
   `{ findings, skipped, failures, sweep_advisories, mode, pr }`: every finding is
   already tagged with its `brief` and `scope` and conforms to the structured
   schema (no parsing needed), `skipped` lists briefs with no files to review,
   `failures` lists any reviewer that returned nothing (a coverage gap — see
   Report), and `sweep_advisories` lists full-extent briefs that were split
   without a declared `sweep` (also see Report).

3. **Triage.** Every finding carries a `code_quote` — the verbatim line(s) the
   reviewer flagged. Use it as a cheap groundedness check: read each finding
   against its quote and drop any where the quote doesn't actually support the
   claim, is empty, or is generic filler. That is the whole of triage — a light
   pass over the returned findings, not a re-review. Do **not** re-read the
   files, spawn verification agents, or rebuild the workflow to "confirm"
   findings; the quote-grounded reviewers plus this spot-check are the guardrail,
   and a fabricated finding shows up as a quote that doesn't match its own title.

4. **Report.** First split the findings on `preexisting`. Findings with
   `preexisting: true` are incidental observations of existing violations the
   change did not introduce — they **never go to the PR**, only to chat.
   - **Full mode**, or **diff mode with no PR**: present a chat summary grouped
     by brief, then by priority (P0 → P3). Lead with the count and any scope
     warnings. (In full mode every finding is just a finding; `preexisting` is a
     diff-mode concept and can be ignored there.)
   - **Diff mode with a PR**: post directly — invoking this skill is the
     authorisation to publish, so do **not** pause to confirm. Post **only the
     change-introduced findings** (those without `preexisting: true`). Assemble
     the findings document and pipe it to the poster:

     ```bash
     echo "$FINDINGS_JSON" | python3 ~/.claude/skills/agent-review/scripts/post_pr_comments.py
     ```

     The document is `{"pr": <number>, "head_sha": "<sha>", "title":
     "Agent Review", "findings": [...]}`, where `head_sha` is `git rev-parse
     HEAD` and each finding carries `file`, `line`, `side`, `priority`, `title`,
     `message`, optional `suggestion`, and `brief`. The `title` sets the review
     heading — set it here because the script's default is deliberately generic
     (it also backs combined runs whose findings aren't all from `.review/`); an
     optional `summary` adds a one-line intro beneath it. The script publishes a
     single PR review (`event: COMMENT`) with that heading as the summary body
     and one inline comment per finding, each headed by a priority badge. If GitHub rejects the batched review because a finding sits
     on a line outside the diff, the script falls back to a body-only review that
     lists every finding. Report what it posted.
   - **Pre-existing findings**, in any mode they appear: report them in chat in
     their own section — *"Pre-existing issues noticed (not posted to the PR)"* —
     grouped by brief, for the user to triage. Do not include them in the
     document piped to the poster.
   - **Skipped briefs**: report the workflow's `skipped` list (brief name and
     reason) so the user knows which concerns were *not* checked this round —
     most often a subtree-scoped brief whose directory had no changes. Skipping
     is correct, but it must be visible: a skipped brief is an unchecked concern,
     not a passed one.
   - **Failed reviewers**: if the workflow returns any `failures` (a reviewer that
     errored or returned nothing), report them — each is a slice of a brief whose
     files went **unreviewed** this run, so that concern is only partly checked.
     Name the affected brief and offer to re-run, the same way a skipped brief is
     surfaced.
   - **Sweep advisories**: if the workflow returns any `sweep_advisories`, surface
     them. Each is a full-extent brief with no declared `sweep` that was split
     across reviewers under the per-file default — fine for a per-file concern, but
     wrong for a cross-file invariant, which needs one reviewer over the whole
     scope. Tell the user, and suggest adding `sweep: whole-tree` to the brief if
     its concern spans files. (`/review-brief` sets `sweep` on new briefs; this
     catches older ones that predate it.)

## Notes

- Reviewers are deliberately narrow and sceptical of their own findings — a
  brief with zero findings against a small diff is normal and correct. Do not
  pad the report to look thorough.
- The scripts assume `gh` is installed and authenticated for PR detection and
  posting. Without it, diff mode still works against the default branch and
  reports to chat.
