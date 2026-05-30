---
name: agent-review
description: Convene a panel of review agents from a repo's `.review/` briefs — one clean-context agent per brief, each scoped to a subtree when its folder matches a repo directory. Use when asked to run an agent review, review the branch/PR against the project's review briefs, or audit the codebase against `.review/`. Diff mode (default) reviews branch changes and posts inline PR comments; full mode audits all in-scope code and reports to chat. Not for general bug-hunting on a diff (use code-review) or for security-only review (use security-review).
argument-hint: "[--full]"
disable-model-invocation: true
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
runs one clean-context agent per brief, each blind to the others, scoped to the
relevant subtree, and collates their findings.

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
  the diff. For concerns only judgeable against the whole tree: global
  invariants, cross-file consistency, "every X is registered in Y". Heavier, so
  reserve it for concerns that genuinely need it.

`/review-brief` decides and sets a brief's extent when authoring; this skill
reads it.

## Runs

| Invocation | What runs | Output |
|------------|-----------|--------|
| `/agent-review` | Every brief, each at its declared extent, vs the branch | Inline PR comments + chat recap; chat-only if no PR |
| `/agent-review <brief>…` | Only the named brief(s), at their declared extent | as above |
| `/agent-review --full [<brief>…]` | The named briefs (or all), forced to full extent | Chat summary grouped by brief |

A default run honours each brief's extent: diff-extent briefs see only changed
files; full-extent briefs audit their whole scope and classify each finding so
change-caused ones still reach the PR (see Report). **`--full`** overrides every
selected brief to a whole-scope audit and reports to chat — use it to baseline a
new brief against existing code (`/agent-review --full <new-brief>`) without
dragging every other brief through a sweep. **Brief selection** — naming briefs —
restricts any run to those briefs.

A full-scope audit enumerates every in-scope tracked file per brief, so it is
heaviest on a repo-wide brief over a large tree — prefer scoping such briefs to
subtree folders.

## Workflow

The fan-out runs as a Workflow so that the dispatch (one agent per brief, every
brief), the per-brief file manifest, and the findings shape are fixed by code
rather than left to the model. The reviewers' judgement stays non-deterministic
— that is the point — but what they are pointed at, and the structure they
return, are not. Invoking the bundled workflow from this skill is the user's
opt-in to multi-agent orchestration — do not ask again before running it.

1. **Plan.** Run the discovery script from the target repo, translating the
   slash arguments: `--full` → `--full`, and any bare words → brief names to
   restrict to.

   ```bash
   python3 ~/.claude/skills/agent-review/scripts/plan_review.py [--full] [brief-name …]
   ```

   Each brief in the plan carries its effective `extent` (`diff` or `full`)
   alongside its `scope` and `target_files`.

   It returns JSON: the briefs, each one's resolved `scope` (or `null` for
   repo-wide), `target_files` (diff mode), `skip_reason`, the detected `pr`,
   the `base_ref`, and `warnings`. If it returns an `error` (no git repo, no
   `.review/`, no briefs), report it and stop. Surface any `warnings` to the
   user — they usually mean a brief is mis-scoped.

2. **Fan out via Workflow.** Read `prompts/review-brief.md` (the stable reviewer
   prompt), then invoke the bundled workflow, passing both the plan and the
   template:

   ```
   Workflow({
     scriptPath: "~/.claude/skills/agent-review/scripts/review_workflow.js",
     args: { plan: <plan JSON from step 1>, promptTemplate: <text of review-brief.md> }
   })
   ```

   The workflow launches one schema-validated agent per non-skipped brief, in
   parallel. Each agent reads only its own brief file and reviews only its
   scoped target — reviewers stay blind to one another. A **diff-extent** brief
   inspects the branch diff for its changed files and flags what the change
   introduces or touches; a **full-extent** brief audits the explicit list of
   in-scope files and classifies each finding as change-caused (`preexisting:
   false`) or independent (`preexisting: true`). A `--full` run forces every
   brief to a plain whole-scope audit instead. It returns
   `{ findings, skipped, mode, pr }`, where every finding is already tagged with
   its `brief` and `scope` and conforms to the structured schema (no parsing or
   malformed-response handling needed on your side).

3. **Triage.** Sanity-check the findings before doing anything with them. Drop
   or flag anything that looks like a misread of the brief or the code rather
   than carrying it through to the report or the PR.

4. **Report.** First split the findings on `preexisting`. Findings with
   `preexisting: true` are incidental observations of existing violations the
   change did not introduce — they **never go to the PR**, only to chat.
   - **Full mode**, or **diff mode with no PR**: present a chat summary grouped
     by brief, then by severity (high → low). Lead with the count and any scope
     warnings. (In full mode every finding is just a finding; `preexisting` is a
     diff-mode concept and can be ignored there.)
   - **Diff mode with a PR**: show the user the collated findings first and
     **confirm before posting** — inline PR comments are outward-facing. Post
     **only the change-introduced findings** (those without `preexisting: true`).
     On confirmation, assemble the findings document and pipe it to the poster:

     ```bash
     echo "$FINDINGS_JSON" | python3 ~/.claude/skills/agent-review/scripts/post_pr_comments.py
     ```

     The document is `{"pr": <number>, "head_sha": "<sha>", "findings": [...]}`,
     where `head_sha` is `git rev-parse HEAD` and each finding carries `file`,
     `line`, `side`, `severity`, `message`, optional `suggestion`, and `brief`.
     The script posts each finding inline (falling back to a summary comment for
     lines outside the diff) plus one summary issue comment. Report what it
     posted.
   - **Pre-existing findings**, in any mode they appear: report them in chat in
     their own section — *"Pre-existing issues noticed (not posted to the PR)"* —
     grouped by brief, for the user to triage. Do not include them in the
     document piped to the poster.
   - **Skipped briefs**: report the workflow's `skipped` list (brief name and
     reason) so the user knows which concerns were *not* checked this round —
     most often a subtree-scoped brief whose directory had no changes. Skipping
     is correct, but it must be visible: a skipped brief is an unchecked concern,
     not a passed one.

## Notes

- Reviewers are deliberately narrow and sceptical of their own findings — a
  brief with zero findings against a small diff is normal and correct. Do not
  pad the report to look thorough.
- The scripts assume `gh` is installed and authenticated for PR detection and
  posting. Without it, diff mode still works against the default branch and
  reports to chat.
