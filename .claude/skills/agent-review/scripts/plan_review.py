#!/usr/bin/env python3
"""Plan an agent-review run: discover briefs, resolve subtree scopes, find targets.

This is the deterministic core of the agent-review skill. It does no reviewing
itself — it works out *what* should be reviewed and *by whom*, then hands a JSON
plan back to the skill, which dispatches clean-context review agents — one per
shard, where a brief with a large file set is split across several reviewers.

Scope resolution mirrors the convention documented in SKILL.md: a brief's
position inside `.review/` determines what it reviews.

  .review/tone.md              -> repo-wide (file sits directly in .review/)
  .review/src/api/limits.md    -> scoped to src/api/ IF that directory exists
  .review/legacy/foo.md        -> repo-wide + warning (no matching repo dir)

The match is on the *full* nested path (src/api, not just api), per the chosen
convention. A mismatch is non-fatal: the brief still runs, scoped repo-wide,
but a warning is surfaced so authors notice typos or stale folder names.
"""

import argparse
import json
import math
import os
import subprocess
import sys


# The shared reviewer method file lives in the skill's prompts/ directory, a
# sibling of this scripts/ directory. Resolve it from THIS file's location so the
# path is correct wherever the skill is vendored (global ~/.claude or a project's
# .claude). Each reviewer agent reads it directly — see review_workflow.js — so
# its multi-KB, backtick-heavy text never has to be relayed through the Workflow
# args, which is brittle for large freeform payloads.
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
TEMPLATE_PATH = os.path.join(os.path.dirname(SCRIPT_DIR), "prompts", "reviewer-method.md")


# --- Sharding policy -------------------------------------------------------
# A brief's review work is a set of files: every tracked file in scope (a full
# audit) or just the changed files in scope (a diff review). Handing one agent a
# very large set produces a shallow review — it skims, or runs out of budget. So
# when the set is large and the brief is shardable, the plan splits it across
# several reviewers, each handed a deterministic slice it enumerates itself (see
# review_workflow.js). That is the Workflow tool used as intended — iterating a
# large group across agents — and it is decided here, in code, not improvised at
# runtime by a reviewer.
#
# SHARD_TARGET_SIZE is the rough upper bound on files per reviewer; at or below
# it, a single reviewer takes the brief (the long-standing behaviour for small
# briefs is unchanged). It is the one knob most worth tuning after a live trial:
# dense source may want fewer files per reviewer, light test files more.
SHARD_TARGET_SIZE = 30
# A ceiling on reviewers per brief, so a pathological repo-wide sweep can't fan
# out without bound. Past this, slices grow beyond SHARD_TARGET_SIZE rather than
# adding agents — coverage stays complete, the agents just get fatter. Set well
# above the Workflow concurrency cap so ordinary large sweeps still parallelise
# fully; it only bites on enormous scopes.
MAX_SHARDS_PER_BRIEF = 24


def run_git(args, cwd):
    """Run a git command, returning stdout stripped. Raises on failure."""
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def try_git(args, cwd):
    """Run a git command, returning stdout or None if it fails."""
    try:
        return run_git(args, cwd)
    except subprocess.CalledProcessError:
        return None


def repo_root():
    """Absolute path to the enclosing git work tree, or None if not in one."""
    try:
        return run_git(["rev-parse", "--show-toplevel"], cwd=os.getcwd())
    except subprocess.CalledProcessError:
        return None


def detect_pr(root):
    """Return PR metadata via gh, or None if there's no PR / gh is unavailable.

    Only the head branch's open PR is relevant; gh resolves it from the current
    branch automatically. Any failure (gh missing, not authed, no PR) collapses
    to None — the caller then falls back to a default-branch diff and chat output.
    """
    try:
        out = subprocess.run(
            ["gh", "pr", "view", "--json", "number,baseRefName,state"],
            cwd=root,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    try:
        data = json.loads(out)
    except json.JSONDecodeError:
        return None
    if not data.get("number"):
        return None
    return {
        "number": data["number"],
        "base_ref": f"origin/{data['baseRefName']}",
        "base_branch": data["baseRefName"],
    }


def default_base(root):
    """Best guess at the base ref to diff against when there's no PR."""
    # origin/HEAD points at the remote's default branch when it's been set.
    head = try_git(["symbolic-ref", "refs/remotes/origin/HEAD"], cwd=root)
    if head:
        return head.replace("refs/remotes/", "")  # e.g. origin/main
    for candidate in ("origin/main", "origin/master", "main", "master"):
        if try_git(["rev-parse", "--verify", "--quiet", candidate], cwd=root) is not None:
            return candidate
    return None


def reviewable(paths):
    """Drop the briefs themselves — `.review/` is configuration, not code to audit."""
    return [p for p in paths if not p.startswith(".review/")]


def tracked_files(root, scope):
    """All git-tracked files within a scope (or the whole repo), respecting .gitignore.

    Used to size a full-extent brief's scope: the count drives how many reviewers
    the brief is split across (see the sharding policy). The reviewers enumerate
    their own slices with the same command, so the list itself is never relayed.
    """
    args = ["ls-files"]
    if scope:
        args += ["--", scope]
    out = try_git(args, cwd=root)
    if out is None:
        return []
    return reviewable(line for line in out.splitlines() if line.strip())


def changed_files(root, base):
    """Files added/modified/renamed/copied between base's merge-base and HEAD.

    Three-dot diff so we compare against the merge-base, not the literal base tip
    — the same set a reviewer cares about. Deletions are excluded: they can't host
    review comments and rarely need brief-level review.
    """
    out = try_git(
        ["diff", "--name-only", "--diff-filter=ACMR", f"{base}...HEAD"],
        cwd=root,
    )
    if out is None:
        return []
    return reviewable(line for line in out.splitlines() if line.strip())


def discover_briefs(review_dir):
    """All markdown briefs under .review/, as paths relative to the repo root's parent."""
    briefs = []
    for current, _dirs, files in os.walk(review_dir):
        for name in sorted(files):
            if name.lower().endswith(".md"):
                briefs.append(os.path.join(current, name))
    return sorted(briefs)


def resolve_scope(brief_path, review_dir, root):
    """Map a brief to its subtree scope.

    Returns (scope, warning) where scope is a repo-relative directory path or
    None for repo-wide. warning is a human-readable string or None.
    """
    rel = os.path.relpath(brief_path, review_dir)
    scope_dir = os.path.dirname(rel)  # "" for a brief sitting directly in .review/
    if not scope_dir:
        return None, None
    scope_dir = scope_dir.replace(os.sep, "/")
    if os.path.isdir(os.path.join(root, scope_dir)):
        return scope_dir, None
    warning = (
        f"Brief '{rel}' is under '.review/{scope_dir}/', but the repo has no "
        f"'{scope_dir}/' directory. Treating it as repo-wide. "
        f"Fix the folder name to scope it, or move the brief if this is intentional."
    )
    return None, warning


def in_scope(path, scope):
    """Does a changed file fall within a brief's scope? None scope means repo-wide."""
    if scope is None:
        return True
    return path == scope or path.startswith(scope + "/")


def parse_frontmatter(brief_path):
    """Read a brief's optional frontmatter for the two keys this skill honours.

    Briefs are prose; frontmatter is optional. We parse only the keys we need by
    hand, so there's no YAML dependency and a malformed block can't break the run.

    Returns (extent, sweep, sweep_explicit, warnings):
      - extent: "diff" (default) or "full" — how much of the scope to read.
        `extent: full` audits the whole scope every run; `diff` (or no
        frontmatter) reviews only what the branch changed.
      - sweep: "per-file" (default) or "whole-tree" — whether the brief can be
        split across reviewers. A per-file brief judges each file on its own, so
        a large set is shared across several reviewers; a whole-tree brief needs
        the whole scope in one view (a cross-file invariant) and is never split.
        Mostly relevant at full extent, where the set is large, but it also keeps
        a brief on a single reviewer when a diff happens to be huge.
      - sweep_explicit: True only when the brief actually declared `sweep`. A
        full-extent brief that did not declare it defaults to per-file and so is
        split when large; the caller uses this to nudge the author toward
        `sweep: whole-tree` if that split would be wrong (a cross-file invariant
        written before `sweep` existed, say).
      - warnings: a present-but-unrecognised value for either key is flagged
        rather than swallowed.
    """
    extent, sweep, warnings = "diff", "per-file", []
    sweep_explicit = False
    try:
        with open(brief_path, encoding="utf-8") as handle:
            if handle.readline().strip() != "---":
                return extent, sweep, sweep_explicit, warnings  # no frontmatter block
            for line in handle:
                if line.strip() == "---":
                    break
                key, sep, value = line.partition(":")
                if not sep:
                    continue
                # Strip an inline YAML comment so a documented example copied
                # verbatim (`extent: full   # …`) still parses; the extent/sweep
                # values never legitimately contain '#'.
                key, raw = key.strip(), value.split("#", 1)[0].strip()
                if key == "extent":
                    if raw.lower() in ("diff", "full"):
                        extent = raw.lower()
                    else:
                        warnings.append(
                            f"Brief '{os.path.basename(brief_path)}' has an unrecognised "
                            f"extent '{raw}'; expected 'diff' or 'full'. Defaulting to diff."
                        )
                elif key == "sweep":
                    if raw.lower() in ("per-file", "whole-tree"):
                        sweep = raw.lower()
                        sweep_explicit = True
                    else:
                        warnings.append(
                            f"Brief '{os.path.basename(brief_path)}' has an unrecognised "
                            f"sweep '{raw}'; expected 'per-file' or 'whole-tree'. "
                            f"Defaulting to per-file."
                        )
    except OSError:
        pass
    return extent, sweep, sweep_explicit, warnings


def plan_shards(file_set_size, shardable):
    """How many reviewers should cover a set of `file_set_size` files.

    Returns 1 when the brief is not shardable (a whole-tree brief, judged as a
    unit) or the set is small enough for one reviewer. Otherwise it is
    ceil(size / SHARD_TARGET_SIZE), capped at MAX_SHARDS_PER_BRIEF.
    """
    if not shardable or file_set_size <= SHARD_TARGET_SIZE:
        return 1
    return min(math.ceil(file_set_size / SHARD_TARGET_SIZE), MAX_SHARDS_PER_BRIEF)


def shard_ranges(file_set_size, shard_count):
    """Split 1..file_set_size into `shard_count` balanced, contiguous 1-based ranges.

    Sizes differ by at most one, so there is no straggler shard left with the
    remainder. Each (start, end) is inclusive, and the ranges tile 1..size
    exactly — every file lands in one shard, none in two — which is what makes
    coverage a property of this arithmetic rather than of an agent's counting. A
    reviewer turns its range into files with `… | sed -n 'start,endp'`.
    """
    base, remainder = divmod(file_set_size, shard_count)
    ranges = []
    start = 1
    for index in range(shard_count):
        # The first `remainder` shards take one extra file to absorb the remainder.
        size = base + (1 if index < remainder else 0)
        end = start + size - 1
        ranges.append((start, end))
        start = end + 1
    return ranges


def build_shards(file_set_size, shardable, extent, target_files):
    """Per-reviewer shard descriptors for one brief — one shard per reviewer.

    Three shapes, matching how the reviewer finds its files (see
    review_workflow.js):
      - {index, total, files}  a single diff reviewer gets the explicit changed
        files inline — the set is small (bounded by the diff), and handing it
        over stops the agent re-deriving a different set with different git flags.
      - {index, total, whole: True}  a single full reviewer enumerates the whole
        scope itself; the list is deliberately not relayed (it can be large, and
        relaying a big list through the Workflow args is what crashed past runs).
      - {index, total, start, end}  a sharded reviewer (diff or full) enumerates
        deterministically and cuts its own 1-based line range; no list relayed.
    """
    count = plan_shards(file_set_size, shardable)
    if count == 1:
        if extent == "diff":
            return [{"index": 1, "total": 1, "files": target_files}]
        return [{"index": 1, "total": 1, "whole": True}]
    return [
        {"index": i + 1, "total": count, "start": start, "end": end}
        for i, (start, end) in enumerate(shard_ranges(file_set_size, count))
    ]


def main():
    parser = argparse.ArgumentParser(description="Plan an agent-review run.")
    parser.add_argument(
        "--mode",
        choices=["diff", "full"],
        default="diff",
        help="diff: honour each brief's extent against the branch changes (default). "
             "full: force every selected brief to audit its whole scope.",
    )
    parser.add_argument(
        "--full",
        action="store_const",
        const="full",
        dest="mode",
        help="Shorthand for --mode full.",
    )
    parser.add_argument(
        "--base",
        metavar="REF",
        default=None,
        help="Branch or ref to diff the current branch against (e.g. origin/develop, "
             "a tag, or a commit). Overrides PR detection and the auto-detected "
             "default branch. Diff mode only; ignored by --full.",
    )
    parser.add_argument(
        "briefs",
        nargs="*",
        help="Optional brief names (filename without .md) to restrict the run to. "
             "Default: every brief. Used to baseline a single new brief.",
    )
    args = parser.parse_args()

    root = repo_root()
    if root is None:
        print(json.dumps({"error": "Not inside a git repository."}))
        sys.exit(1)

    review_dir = os.path.join(root, ".review")
    if not os.path.isdir(review_dir):
        print(json.dumps({"error": f"No .review/ directory found at repo root ({root})."}))
        sys.exit(1)

    brief_paths = discover_briefs(review_dir)
    if not brief_paths:
        print(json.dumps({"error": "No .md briefs found under .review/."}))
        sys.exit(1)

    # The reviewer method file must exist — every reviewer reads it. A missing
    # file is a broken skill install, not a repo problem; fail clearly here
    # rather than letting every reviewer agent fail to read it later.
    if not os.path.isfile(TEMPLATE_PATH):
        print(json.dumps({"error": f"Reviewer method file not found at {TEMPLATE_PATH}."}))
        sys.exit(1)

    warnings = []
    plan = {
        "repo_root": root,
        "template_path": TEMPLATE_PATH,
        "mode": args.mode,
        "pr": None,
        "base_ref": None,
        "base_source": None,  # "explicit" | "pr" | "default" — set in diff mode
        "warnings": warnings,
        "briefs": [],
    }

    # Optional brief selection: restrict the run to named briefs (by filename
    # stem). Used to baseline a single new brief without re-auditing the rest.
    selected = {name.lower() for name in args.briefs}
    matched = set()

    # A diff run needs the changed-file set and base ref. They feed diff-extent
    # briefs directly, and full-extent briefs use the base ref to classify each
    # finding as change-caused (→ PR) or independent (→ chat).
    changed = None
    if args.mode == "diff":
        pr = detect_pr(root)
        # Base resolution, most explicit first: an explicit --base wins outright;
        # otherwise a detected PR's base; otherwise a guess at the default branch.
        # We record which, so the skill can present a *guessed* base as provisional
        # (and point the user at --base) rather than reporting it as authoritative —
        # the failure that hurts most is a wrong base reading as "nothing changed".
        if args.base:
            base, base_source = args.base, "explicit"
        elif pr:
            base, base_source = pr["base_ref"], "pr"
        else:
            base, base_source = default_base(root), "default"

        if base is None:
            print(json.dumps({
                "error": "Could not determine a branch to diff against — no PR, and no "
                         "origin/HEAD, main, or master found. Pass --base <ref> to name "
                         "the branch this work forks from, or run with --full."
            }))
            sys.exit(1)
        # An explicit --base that doesn't resolve is a user error worth catching
        # loudly, rather than letting `git diff` yield a confusing empty result.
        if base_source == "explicit" and try_git(
            ["rev-parse", "--verify", "--quiet", base], cwd=root
        ) is None:
            print(json.dumps({
                "error": f"--base '{base}' is not a valid ref in this repository."
            }))
            sys.exit(1)

        plan["pr"] = pr
        plan["base_ref"] = base
        plan["base_source"] = base_source
        changed = changed_files(root, base)
        if not changed:
            # A guessed base finding nothing may mean a clean branch — or the wrong
            # base. Say so, so the user reads it as a question, not an all-clear.
            hint = (
                " (base auto-detected; pass --base <ref> if that's the wrong branch)"
                if base_source == "default"
                else ""
            )
            warnings.append(f"No changed files between {base} and HEAD{hint}.")

    for brief_path in brief_paths:
        name = os.path.splitext(os.path.basename(brief_path))[0]
        if selected and name.lower() not in selected:
            continue
        matched.add(name.lower())

        scope, scope_warning = resolve_scope(brief_path, review_dir, root)
        if scope_warning:
            warnings.append(scope_warning)

        extent, sweep, sweep_explicit, fm_warnings = parse_frontmatter(brief_path)
        warnings.extend(fm_warnings)
        # --full forces a whole-scope audit of every selected brief, overriding
        # the brief's own declared extent. sweep (shardability) still applies.
        effective_extent = "full" if args.mode == "full" else extent

        rel_brief = os.path.relpath(brief_path, root)
        entry = {
            "path": rel_brief,
            "name": name,
            "scope": scope,              # None == repo-wide
            "extent": effective_extent,  # "diff" or "full"
            "sweep": sweep,              # "per-file" or "whole-tree"
            "skip_reason": None,
        }

        scope_label = scope or "the whole repo"
        # The set of files this brief reviews: every tracked file in scope for a
        # full audit, or just the changed files in scope for a diff review. We
        # size that set here; the reviewers enumerate the actual paths themselves
        # (full extent, or any sharded brief) or get the small changed list inline
        # (an unsharded diff brief) — see build_shards and review_workflow.js.
        if effective_extent == "full":
            target_files = None
            file_set_size = len(tracked_files(root, scope))
            empty_reason = f"No tracked files in scope ({scope_label})."
        else:
            target_files = [f for f in changed if in_scope(f, scope)]
            file_set_size = len(target_files)
            empty_reason = f"No changed files in scope ({scope_label})."

        entry["file_set_size"] = file_set_size
        if file_set_size == 0:
            entry["skip_reason"] = empty_reason
        else:
            # A per-file brief can be split across reviewers when the set is large;
            # a whole-tree brief (a cross-file invariant) must be judged as a unit,
            # so it is never split however big its scope.
            shardable = sweep == "per-file"
            entry["shards"] = build_shards(
                file_set_size, shardable, effective_extent, target_files
            )
            # A brief that *declares* full extent but no sweep defaults to per-file
            # and so is split when large. If it is really a cross-file invariant
            # (authored before `sweep` existed, say), that split is wrong — flag it
            # once it is actually split, so the skill can nudge the author toward
            # `sweep: whole-tree`. Keyed on the brief's own declared extent, not a
            # --full override, so a plain diff brief forced --full isn't nagged.
            entry["sweep_advisory"] = (
                extent == "full" and not sweep_explicit and len(entry["shards"]) > 1
            )

        plan["briefs"].append(entry)

    # Surface requested briefs that matched nothing; error only if none matched
    # at all, so a single typo in a multi-brief request doesn't run the wrong set.
    if selected:
        for name in sorted(selected - matched):
            warnings.append(f"Requested brief '{name}' matched no .md file under .review/.")
        if not matched:
            print(json.dumps({
                "error": "None of the requested briefs matched a .md file under .review/: "
                         + ", ".join(sorted(selected))
            }))
            sys.exit(1)

    print(json.dumps(plan, indent=2))


if __name__ == "__main__":
    main()
