# Reviewer method

This is the shared working method and findings format for the agent-review panel.
Every reviewer follows it, whatever their brief. Your specific brief, scope, and
review target are in the task prompt that dispatched you — this file is only
*how* to review and *how* to report, not *what* to review.

## How to work

1. Read the brief, then explore the in-scope code as needed to judge it against
   the brief. Read the files, follow relevant references, understand the
   context. Do not assume — verify against what the code actually does.
2. Quote the code first, judge it second. Before you raise a finding, copy the
   exact line or lines it concerns — verbatim, from the file you just read, not
   from memory and not paraphrased — and state the finding *from* that quote.
   This is the discipline that keeps a review grounded: a claim you can't tie to
   text you actually read is exactly where reviews drift into citing functions,
   fields, or line numbers that aren't there.
3. If you cannot produce that quote — the symbol, line, or code you would cite
   turns out not to exist — do not raise the finding. A finding you can't ground
   in real code is a misread on your part; drop it silently. "I checked and
   found nothing here" is always a valid and useful result.
4. Raise a finding only when the code genuinely fails or risks failing the
   brief, and you can point to the specific place. Cite the file and the line as
   it appears in the current version.
5. Be honest about confidence. A speculative concern is worse than silence — if
   you are not reasonably sure the brief is violated, leave it out.
6. Do not invent issues to look productive. Zero findings is a valid, common
   result for a focused brief against a small change.

## Reporting your findings

You will return your findings as structured output. For each finding give:

- `file` — path relative to the repo root.
- `line` — the line the finding refers to, in the new/current version of the
  file. Omit only if the finding genuinely has no single location.
- `code_quote` — the verbatim line or lines from the file that the finding
  concerns, copied exactly as they appear. For a finding about something
  *missing*, quote the code at the place it should appear and say what is
  absent. This is required: it is the evidence the finding is real and correctly
  located. If you have nothing concrete to quote, you do not have a finding —
  leave it out.
- `side` — `RIGHT` for a line in the current version (the usual case) or `LEFT`
  for a removed line.
- `priority` — how much the finding matters:
  - `P0` — blocks merge: data loss, a security issue, or a serious regression.
  - `P1` — should be fixed before merge.
  - `P2` — a valid issue, but not necessarily merge-blocking.
  - `P3` — a minor cleanup or maintainability note.
- `title` — a short imperative summary of the finding (one line, no trailing
  punctuation), e.g. "Render placeholders for forked agent prompts".
- `message` — specific and grounded in the code you read, not generic advice:
  what is wrong and why it violates the brief. This is the body beneath the
  title, so it may run to a couple of short paragraphs.
- `suggestion` — a concrete fix, or omit if not obvious.
- `preexisting` — only relevant when the scope instructions describe a diff
  review. Set it `true` for a violation you incidentally noticed that the change
  did not introduce; otherwise omit it. Follow the scope instructions on when to
  flag these.

Set `brief` to the name of the brief you were given, and use `notes` for one
line on what you checked, especially when there are no findings.
