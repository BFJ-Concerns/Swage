#!/usr/bin/env python3
"""Plan an agent-review run: discover briefs, resolve subtree scopes, find targets.

This is the deterministic core of the agent-review skill. It does no reviewing
itself — it works out *what* should be reviewed and *by whom*, then hands a JSON
plan back to the skill, which dispatches one clean-context agent per brief.

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
import os
import subprocess
import sys


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

    Used by full mode to hand each reviewer an explicit manifest of what to audit,
    so coverage is a property of the plan rather than of how far an agent wanders.
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


def parse_extent(brief_path):
    """Read a brief's optional frontmatter for its review extent.

    Briefs are prose; frontmatter is optional. We parse only the one key we need
    (`extent`) by hand, so there's no YAML dependency and a malformed block can't
    break the run. Returns (extent, warning): extent is "diff" (the default) or
    "full"; warning flags a present-but-unrecognised value rather than swallowing
    it. `extent: full` means the brief audits its whole scope every run; `diff`
    (or no frontmatter) means it reviews only what the branch changed.
    """
    try:
        with open(brief_path, encoding="utf-8") as handle:
            if handle.readline().strip() != "---":
                return "diff", None  # no frontmatter block
            for line in handle:
                if line.strip() == "---":
                    break
                key, sep, value = line.partition(":")
                if sep and key.strip() == "extent":
                    extent = value.strip().lower()
                    if extent in ("diff", "full"):
                        return extent, None
                    return "diff", (
                        f"Brief '{os.path.basename(brief_path)}' has an unrecognised "
                        f"extent '{value.strip()}'; expected 'diff' or 'full'. "
                        f"Defaulting to diff."
                    )
    except OSError:
        pass
    return "diff", None


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

    warnings = []
    plan = {
        "repo_root": root,
        "mode": args.mode,
        "pr": None,
        "base_ref": None,
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
        base = pr["base_ref"] if pr else default_base(root)
        if base is None:
            print(json.dumps({
                "error": "Could not determine a base branch to diff against. "
                         "Specify one or run with --full."
            }))
            sys.exit(1)
        plan["pr"] = pr
        plan["base_ref"] = base
        changed = changed_files(root, base)
        if not changed:
            warnings.append(f"No changed files between {base} and HEAD.")

    for brief_path in brief_paths:
        name = os.path.splitext(os.path.basename(brief_path))[0]
        if selected and name.lower() not in selected:
            continue
        matched.add(name.lower())

        scope, scope_warning = resolve_scope(brief_path, review_dir, root)
        if scope_warning:
            warnings.append(scope_warning)

        extent, extent_warning = parse_extent(brief_path)
        if extent_warning:
            warnings.append(extent_warning)
        # --full forces a whole-scope audit of every selected brief, overriding
        # the brief's own declared extent.
        effective_extent = "full" if args.mode == "full" else extent

        rel_brief = os.path.relpath(brief_path, root)
        entry = {
            "path": rel_brief,
            "name": name,
            "scope": scope,              # None == repo-wide
            "extent": effective_extent,  # "diff" or "full"
            "skip_reason": None,
            "target_files": None,
        }

        scope_label = scope or "the whole repo"
        if effective_extent == "full":
            # Audit the whole scope: enumerate its files so coverage is explicit.
            targets = tracked_files(root, scope)
            entry["target_files"] = targets
            if not targets:
                entry["skip_reason"] = f"No tracked files in scope ({scope_label})."
        else:
            # Diff extent: only what the branch changed within the scope.
            targets = [f for f in changed if in_scope(f, scope)]
            entry["target_files"] = targets
            if not targets:
                entry["skip_reason"] = f"No changed files in scope ({scope_label})."

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
