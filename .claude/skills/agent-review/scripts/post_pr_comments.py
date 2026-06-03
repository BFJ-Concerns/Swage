#!/usr/bin/env python3
"""Post collated agent-review findings to a GitHub PR as a single review.

Reads a findings JSON document — from a file path argument when given, otherwise
stdin (the shape the skill assembles from each reviewer agent's output) — and
publishes it as one Pull Request Review: a
top-level summary body plus one inline review comment per finding, in the
Codex-connector layout.

Atomic-failure caveat: GitHub rejects a whole review if *any* inline comment
references a line that isn't part of the diff. We submit the batched review
anyway (the skill asks for a single review object), but if that call is
rejected we fall back to re-posting the review with the body only and listing
every finding in the body text — so a single out-of-diff line degrades the
output rather than sinking the run.

The heading and intro are caller-supplied (`title`, `summary`) rather than
fixed, because the same script also backs combined runs whose findings aren't
all from `.review/` briefs. Both are optional; the default heading makes no
claim about where the findings came from.

Expected document shape:

  {
    "pr": 123,
    "head_sha": "abc123...",
    "title": "Agent Review",         # optional; defaults to "Review"
    "summary": "One-line intro.",    # optional; omitted if absent
    "findings": [
      {"brief": "tone", "scope": "src/api", "file": "src/api/x.ts",
       "line": 42, "side": "RIGHT", "priority": "P1",
       "title": "Short imperative summary",
       "message": "...", "suggestion": "..."}
    ]
  }

`gh` must be installed and authenticated; the {owner}/{repo} placeholders are
resolved by gh from the current repository.
"""

import json
import subprocess
import sys

# Lower number sorts first (most urgent). Anything unrecognised sorts last.
PRIORITY_ORDER = {"P0": 0, "P1": 1, "P2": 2, "P3": 3}

# shields.io badge colour per priority. The badge is rendered inline in each
# review comment header.
PRIORITY_COLOUR = {"P0": "red", "P1": "orange", "P2": "yellow", "P3": "blue"}


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


def priority_of(finding):
    return (finding.get("priority") or "P2").upper()


def badge(priority):
    """Inline shields.io badge for a priority level."""
    colour = PRIORITY_COLOUR.get(priority, "lightgrey")
    return f"![{priority}](https://img.shields.io/badge/{priority}-{colour}?style=flat)"


def format_comment(finding):
    """Inline review-comment body for a single finding."""
    priority = priority_of(finding)
    title = (finding.get("title") or finding.get("brief", "review")).strip()
    # The badge is double-subscripted so it sits small against the title text,
    # matching the connector style.
    parts = [f"**<sub><sub>{badge(priority)}</sub></sub>  {title}**", ""]
    parts.append(finding.get("message", "").strip())
    suggestion = finding.get("suggestion")
    if suggestion:
        parts.append("")
        parts.append(suggestion.strip())
    return "\n".join(parts)


def format_body(head_sha, listed, title, summary):
    """Top-level review body. `title` is the review heading and `summary` an
    optional one-line intro beneath it — both supplied by the caller, since the
    same script also serves combined runs whose findings aren't all from
    `.review/`. `listed` is the findings to enumerate in the body text — only
    those that couldn't be attached inline on the happy path, or every finding
    on the body-only fallback path."""
    short_sha = head_sha[:7] if head_sha else "?"
    lines = [f"### {title}", ""]
    if summary:
        lines += [summary, ""]
    lines += [f"**Reviewed commit:** `{short_sha}`"]

    if listed:
        lines += ["", "---", ""]
        by_brief = {}
        for f in listed:
            by_brief.setdefault(f.get("brief", "review"), []).append(f)
        for brief in sorted(by_brief):
            items = sorted(
                by_brief[brief],
                key=lambda f: PRIORITY_ORDER.get(priority_of(f), 9),
            )
            lines.append(f"#### {brief}")
            for f in items:
                loc = f.get("file", "?")
                if f.get("line"):
                    loc += f":{f['line']}"
                title = (f.get("title") or f.get("message", "")).strip()
                lines.append(f"- **{priority_of(f)}** `{loc}` — {title}")
            lines.append("")

    return "\n".join(lines).rstrip()


def post_review(pr, head_sha, body, comments):
    """Publish a single PR review. Returns (ok, error_text)."""
    payload = {"commit_id": head_sha, "event": "COMMENT", "body": body}
    if comments:
        payload["comments"] = comments
    return gh_api(f"repos/{{owner}}/{{repo}}/pulls/{pr}/reviews", payload)


def load_document():
    """Read the findings JSON from a file path argument if given, else stdin.

    A path argument lets the skill run `python3 …post_pr_comments.py <file>` — a
    command that begins with `python3`, so it matches the skill's allowed-tools
    `Bash(python3 *post_pr_comments.py*)` pattern and posts without a permission
    prompt (and sidesteps shell-quoting a large JSON blob). Reading stdin is still
    supported, e.g. a combined routine that pipes a document it assembled itself.
    """
    if len(sys.argv) > 1:
        path = sys.argv[1]
        try:
            with open(path, encoding="utf-8") as handle:
                return json.load(handle)
        except OSError as exc:
            print(f"Could not read findings file '{path}': {exc}", file=sys.stderr)
            sys.exit(1)
        except json.JSONDecodeError as exc:
            print(f"Invalid findings JSON in '{path}': {exc}", file=sys.stderr)
            sys.exit(1)
    try:
        return json.load(sys.stdin)
    except json.JSONDecodeError as exc:
        print(f"Invalid findings JSON on stdin: {exc}", file=sys.stderr)
        sys.exit(1)


def main():
    doc = load_document()

    pr = doc.get("pr")
    head_sha = doc.get("head_sha")
    findings = doc.get("findings", [])
    # Heading and intro are caller-supplied so a combined run can label the
    # review for itself; the default makes no claim about where findings came from.
    title = doc.get("title") or "Review"
    summary = doc.get("summary")
    if not pr or not head_sha:
        print("findings JSON must include 'pr' and 'head_sha'.", file=sys.stderr)
        sys.exit(1)

    # A finding with no concrete line can't be inlined; it goes in the body list.
    inlineable = [f for f in findings if f.get("file") and f.get("line")]
    leftovers = [f for f in findings if not (f.get("file") and f.get("line"))]

    comments = [
        {
            "path": f["file"],
            "line": f["line"],
            "side": f.get("side", "RIGHT"),
            "body": format_comment(f),
        }
        for f in inlineable
    ]

    # Happy path: body lists only the findings that couldn't be inlined.
    ok, err = post_review(
        pr, head_sha, format_body(head_sha, leftovers, title, summary), comments
    )
    posted_inline = len(comments)
    fell_back = False

    if not ok and comments:
        # Most likely an inline comment referenced a line outside the diff, which
        # fails the whole review atomically. Re-post body-only, listing every
        # finding in the body so nothing is lost.
        fell_back = True
        posted_inline = 0
        ok, err = post_review(
            pr, head_sha, format_body(head_sha, findings, title, summary), []
        )

    result = {
        "total": len(findings),
        "posted_inline": posted_inline,
        "in_body_only": len(findings) - posted_inline,
        "review_posted": ok,
        "fell_back_to_body_only": fell_back,
    }
    if not ok:
        result["error"] = err
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
