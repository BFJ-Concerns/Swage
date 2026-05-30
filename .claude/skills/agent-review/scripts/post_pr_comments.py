#!/usr/bin/env python3
"""Post collated agent-review findings to a GitHub PR.

Reads a findings JSON document on stdin (the shape the skill assembles from each
reviewer agent's output) and posts it to the PR as inline review comments, plus
one summary issue comment.

Findings are posted individually rather than as a single batched review on
purpose: GitHub rejects an inline comment whose line isn't part of the diff, and
a batched review fails atomically if *any* comment is invalid. Posting one at a
time means a single out-of-diff finding degrades to the summary rather than
sinking the whole run.

Expected stdin shape:

  {
    "pr": 123,
    "head_sha": "abc123...",
    "findings": [
      {"brief": "tone", "scope": "src/api", "file": "src/api/x.ts",
       "line": 42, "side": "RIGHT", "severity": "high",
       "message": "...", "suggestion": "..."}
    ]
  }

`gh` must be installed and authenticated; the {owner}/{repo} placeholders are
resolved by gh from the current repository.
"""

import json
import subprocess
import sys

SEVERITY_ORDER = {"high": 0, "medium": 1, "low": 2}


def gh_api(endpoint, payload):
    """POST JSON to a gh api endpoint. Returns (ok, response_or_error_text)."""
    try:
        result = subprocess.run(
            ["gh", "api", "--method", "POST", endpoint, "--input", "-"],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        return False, "gh CLI not found on PATH."
    if result.returncode != 0:
        return False, result.stderr.strip() or result.stdout.strip()
    return True, result.stdout


def format_comment(finding):
    """Inline comment body for a single finding."""
    severity = finding.get("severity", "info").lower()
    brief = finding.get("brief", "review")
    body = f"**[{severity}] {brief}**\n\n{finding.get('message', '').strip()}"
    suggestion = finding.get("suggestion")
    if suggestion:
        body += f"\n\n*Suggestion:* {suggestion.strip()}"
    return body


def format_summary(findings, posted, leftovers):
    """Issue-comment body summarising the run and listing anything not inlined."""
    lines = ["## Agent review", ""]
    if not findings:
        lines.append("No findings. The briefs are satisfied for the reviewed changes.")
        return "\n".join(lines)

    by_brief = {}
    for f in findings:
        by_brief.setdefault(f.get("brief", "review"), []).append(f)

    lines.append(
        f"{len(findings)} finding(s) across {len(by_brief)} brief(s) "
        f"— {posted} posted inline."
    )
    lines.append("")
    for brief in sorted(by_brief):
        items = sorted(
            by_brief[brief],
            key=lambda f: SEVERITY_ORDER.get(f.get("severity", "low").lower(), 9),
        )
        lines.append(f"### {brief}")
        for f in items:
            loc = f.get("file", "?")
            if f.get("line"):
                loc += f":{f['line']}"
            lines.append(f"- **[{f.get('severity', 'info')}]** {loc} — {f.get('message', '').strip()}")
        lines.append("")

    if leftovers:
        lines.append("### Findings not attached inline")
        lines.append(
            "_These referenced lines outside the diff, so they couldn't be posted "
            "as inline comments:_"
        )
        for f in leftovers:
            loc = f.get("file", "?")
            if f.get("line"):
                loc += f":{f['line']}"
            lines.append(f"- **[{f.get('severity', 'info')}] {f.get('brief', 'review')}** {loc} — {f.get('message', '').strip()}")
        lines.append("")

    return "\n".join(lines)


def main():
    try:
        doc = json.load(sys.stdin)
    except json.JSONDecodeError as exc:
        print(f"Invalid findings JSON on stdin: {exc}", file=sys.stderr)
        sys.exit(1)

    pr = doc.get("pr")
    head_sha = doc.get("head_sha")
    findings = doc.get("findings", [])
    if not pr or not head_sha:
        print("findings JSON must include 'pr' and 'head_sha'.", file=sys.stderr)
        sys.exit(1)

    posted = 0
    leftovers = []
    for finding in findings:
        # A finding with no concrete line can't be inlined; route it to the summary.
        if not finding.get("file") or not finding.get("line"):
            leftovers.append(finding)
            continue
        payload = {
            "body": format_comment(finding),
            "commit_id": head_sha,
            "path": finding["file"],
            "line": finding["line"],
            "side": finding.get("side", "RIGHT"),
        }
        ok, _ = gh_api(f"repos/{{owner}}/{{repo}}/pulls/{pr}/comments", payload)
        if ok:
            posted += 1
        else:
            # Most failures here are "line not in diff" (422). Fall back gracefully.
            leftovers.append(finding)

    summary_ok, summary_err = gh_api(
        f"repos/{{owner}}/{{repo}}/issues/{pr}/comments",
        {"body": format_summary(findings, posted, leftovers)},
    )

    result = {
        "total": len(findings),
        "posted_inline": posted,
        "in_summary_only": len(leftovers),
        "summary_comment_posted": summary_ok,
    }
    if not summary_ok:
        result["summary_error"] = summary_err
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
