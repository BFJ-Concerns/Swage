#!/usr/bin/env bash
#
# signoff-page.sh — publish, revise, watch, and revoke a commission sign-off
# page. Procedure and doctrine: references/signoff-page.md.
#
# Subcommands:
#
#   publish  --diff <file> [--readme <file>] [--decisions-file <file>]
#            [--title <text>] [--intro-file <file>]
#   publish  --document <file> [--decisions-file <file>] [--title <text>] [--intro-file <file>]
#            Publish a diff (optionally with the full README behind an
#            expander), or a whole document. Prints
#            {"page_id","url","responses_seen"}. The daemon shows the URL
#            once — hand it to the user immediately.
#            --decisions-file: the round's decision points (blank-line-
#            separated items; each item's first line is its headline, the
#            rest its detail). The page then leads with them — sized ask
#            first, points ranked as given, the full change collapsed below —
#            instead of opening on the raw artifact.
#
#   edit <page-id>  (same content flags as publish)
#            Replace the page in place; the URL and collected responses
#            survive. Prints the same JSON; its "responses_seen" excludes
#            the new round.
#
#   watch <page-id> --seen <n> [--timeout-secs <n>] [--interval-secs <n>]
#            Poll until a response newer than --seen arrives; print
#            {"verdict","comments","submitted_at_unix"} and exit 0. On its
#            own deadline: {"verdict":"timeout"}, exit 3.
#
#   revoke <page-id>
#
# Exit codes: 0 done; 3 watch timeout; 4 pages unavailable on this machine
# (client or deployment); 1 anything else.

set -euo pipefail

STATE_DIR="$HOME/.wrack-client"

die() { printf 'signoff-page: %s\n' "$1" >&2; exit 1; }
unavailable() { printf 'signoff-page: %s\n' "$1" >&2; exit 4; }

# The client is a per-machine operator installation; absence means pages are
# unavailable here, a state the caller falls back from rather than repairs.
command -v wrack >/dev/null 2>&1 || unavailable "wrack client not on PATH — pages unavailable on this machine"
[ -d "$STATE_DIR" ] || unavailable "$STATE_DIR missing — this machine is not connected to the deployment"
command -v jq >/dev/null 2>&1 || die "jq is required"

wrack_cmd() { wrack --state-dir "$STATE_DIR" "$@"; }

# wrack_try <outfile> <args...>: run a wrack command with stdout captured to
# a file, and classify failure — exit 4 on transport errors (an unreachable
# deployment is the documented fall-back-to-chat case), exit 1 otherwise (a
# usage error the caller should fix, not fall back from).
wrack_try() {
  local out="$1"; shift
  local err
  if ! err="$(wrack_cmd "$@" 2>&1 >"$out")"; then
    case "$err" in
      *onnection*|*"ould not resolve"*|*"etwork is unreachable"*|*"imed out"*|*nreachable*|*"dns error"*)
        printf 'signoff-page: deployment unreachable: %s\n' "$err" >&2; exit 4 ;;
      *)
        printf 'signoff-page: wrack failed: %s\n' "$err" >&2; exit 1 ;;
    esac
  fi
}

html_escape_file() { jq -Rrs '@html' < "$1"; }

# --- Compose the verbatim document ------------------------------------------
#
# Published with --verbatim because Wrack's themed mode rejects all styling
# and a diff wants the classic per-line red/green highlighting. The document
# still wears the real Wrack theme: page subdomains serve /__wrack/wrack.css
# and its fonts (same daemon, same origin — the assets cannot be missing
# while the page itself loads), so the head links that stylesheet and the
# body replicates the themed wrapper markup, and only the diff-line rules
# are added locally. Those rules colour with the theme's own tokens — the
# --error red family for removals, the --status-foreign-hue teal for
# additions (the theme deliberately has no green; teal is its green-family
# status hue) — so both modes come from the theme's variables. Every content
# line is escaped, so the user still signs the exact bytes to be committed.
# The form field names and verdict values are the handles the watch depends
# on — change them nowhere without changing both.

# Classify each diff line and emit it as a highlighted row. File headers
# (+++/---) and other metadata are tested before the +/- content lines they
# would otherwise match.
diff_lines_html() {
  jq -Rrs '
    sub("\n$"; "")
    | split("\n")
    | map(
        (if startswith("+++ ") or startswith("--- ") or startswith("diff ")
            or startswith("index ") or startswith("new file") or startswith("deleted file")
            or startswith("old mode") or startswith("new mode")
            or startswith("rename ") or startswith("similarity ") or startswith("copy ")
            or startswith("Binary files") or startswith("\\")
         then "meta"
         elif startswith("@@") then "hunk"
         elif startswith("+") then "add"
         elif startswith("-") then "del"
         else "ctx" end) as $class
        | "<code class=\"line \($class)\">" + (if length == 0 then " " else @html end) + "</code>"
      )
    # Joined without separators: the .line blocks break lines themselves, and
    # inside a <pre> a literal newline between them would render as a blank
    # line.
    | join("")
  ' < "$1"
}

compose() {
  local mode="$1" content_file="$2" readme_file="$3" title="$4" intro_file="$5" decisions_file="$6" out="$7"
  local title_html
  title_html="$(printf '%s' "$title" | jq -Rrs '@html')"
  local decision_count=0
  if [ -n "$decisions_file" ]; then
    decision_count="$(jq -Rrs '[split("\n\n")[] | select(length > 0)] | length' < "$decisions_file")"
  fi

  {
    printf '<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n<meta name="viewport" content="width=device-width, initial-scale=1">\n<title>%s</title>\n' "$title_html"
    cat <<'EOF'
<link rel="icon" type="image/svg+xml" href="/__wrack/favicon.svg">
<link rel="stylesheet" href="/__wrack/wrack.css">
<style>
  /* Diff-line rules only; everything else is the Wrack theme. Removals wear
     the theme's error red, additions its teal status hue (the palette has no
     green — teal is its green-family colour), hunk headers the soft ink. */
  .published-page__content pre.diff {
    padding: var(--space-2) 0;
    background: var(--paper);
    line-height: 1.55;
  }
  .diff .line {
    display: block;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    padding: 0 var(--space-4);
  }
  .line.add {
    color: color-mix(in srgb, var(--status-foreign-hue) 80%, var(--ink));
    background: color-mix(in srgb, var(--status-foreign-hue) 14%, var(--paper));
  }
  .line.del {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 12%, var(--paper));
  }
  .line.hunk {
    color: var(--ink-soft);
    background: var(--paper-deep);
    margin-top: var(--space-2);
  }
  .line.meta { color: var(--ink-faint); }
  .published-page__content pre.doc { white-space: pre-wrap; overflow-wrap: anywhere; }
  .published-page__content summary { cursor: pointer; color: var(--ink-soft); }
  .published-page__content details[open] > summary { margin-bottom: var(--space-3); }
  /* The theme paints every button accent-orange; keep that for the primary
     Sign off and demote the other verdicts to an ink outline so the
     opposing choices don't look interchangeable. */
  .published-page__content button[value="revise"],
  .published-page__content button[value="approve-skim"] {
    color: var(--ink);
    background: var(--paper);
    box-shadow: var(--offset) var(--offset) 0 0 var(--rule-soft);
  }
  .published-page__content button[value="revise"]:hover,
  .published-page__content button[value="approve-skim"]:hover {
    color: var(--paper);
    background: var(--ink);
  }
  /* Decision points lead the page: one card each, headline first. */
  .published-page__content .decision {
    padding: var(--space-3) var(--space-4);
    border-left: 3px solid var(--rule-soft);
    margin-bottom: var(--space-3);
  }
  .published-page__content .decision > strong { display: block; }
  .published-page__content .decision p { margin: var(--space-1) 0 0; }
  .published-page__content .ask-size { color: var(--ink-soft); }
</style>
</head>
<body class="published-page-body">
<main class="published-page wrack-container">
<article class="published-page__content">
EOF
    printf '<h1>%s</h1>\n' "$title_html"

    if [ -n "$intro_file" ]; then
      # Plain text, blank-line-separated paragraphs, shown above the standard
      # explanation — context for this round, not a replacement for it.
      jq -Rrs 'split("\n\n") | map(select(length > 0) | "<p>" + @html + "</p>") | join("\n")' < "$intro_file"
      echo
    fi
    printf '<p>Commission sign-off. <strong>Sign off</strong> approves this change — it will be committed and pushed. <strong>Signed on a skim</strong> approves too, honestly recorded as a light read. <strong>Request changes</strong> brings your comments back into the conversation. Either way, add comments below if you have them.</p>\n'

    if [ "$decision_count" -gt 0 ]; then
      # Decisions-first layout: the sized ask, then one card per point, then
      # the full artifact behind an expander — the entry point is the small
      # set of judgements this round actually needs, not the whole change.
      printf '<h2>What needs your judgement</h2>\n'
      printf '<p class="ask-size">%s decision point(s) this round — the full change is below if you want it.</p>\n' "$decision_count"
      jq -Rrs '
        [split("\n\n")[] | select(length > 0)]
        | map(
            split("\n") as $lines
            | "<div class=\"decision\"><strong>" + ($lines[0] | @html) + "</strong>"
              + (if ($lines | length) > 1
                 then "<p>" + ($lines[1:] | join("\n") | @html) + "</p>"
                 else "" end)
              + "</div>"
          )
        | join("\n")
      ' < "$decisions_file"
      echo
    fi

    if [ "$mode" = "diff" ]; then
      if [ "$decision_count" -gt 0 ]; then
        printf '<details><summary>The full change (diff)</summary>\n<pre class="diff">'
        diff_lines_html "$content_file"
        printf '</pre>\n</details>\n'
      else
        printf '<h2>The change</h2>\n<pre class="diff">'
        diff_lines_html "$content_file"
        printf '</pre>\n'
      fi
      if [ -n "$readme_file" ]; then
        printf '<details><summary>The full README as it will stand</summary>\n<pre class="doc">'
        html_escape_file "$readme_file"
        printf '</pre>\n</details>\n'
      fi
    else
      if [ "$decision_count" -gt 0 ]; then
        printf '<details><summary>The full commission</summary>\n<pre class="doc">'
        html_escape_file "$content_file"
        printf '</pre>\n</details>\n'
      else
        printf '<h2>The commission</h2>\n<pre class="doc">'
        html_escape_file "$content_file"
        printf '</pre>\n'
      fi
    fi

    cat <<'EOF'
<h2>Your verdict</h2>
<form method="post">
  <p><label for="comments">Comments — optional with a sign-off; for a revision, say what to change:</label></p>
  <p><textarea id="comments" name="comments" rows="8" cols="80"></textarea></p>
  <p>
    <button type="submit" name="verdict" value="approve">Sign off</button>
    <button type="submit" name="verdict" value="approve-skim">Signed on a skim</button>
    <button type="submit" name="verdict" value="revise">Request changes</button>
  </p>
</form>
</article>
</main>
</body>
</html>
EOF
  } > "$out"
}

# --- Shared flag parsing for publish/edit -----------------------------------

parse_content_flags() {
  mode="" content_file="" readme_file="" title="Commission sign-off" intro_file="" decisions_file=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --diff)           mode="diff";     content_file="${2:-}"; shift 2 ;;
      --document)       mode="document"; content_file="${2:-}"; shift 2 ;;
      --readme)         readme_file="${2:-}"; shift 2 ;;
      --title)          title="${2:-}"; shift 2 ;;
      --intro-file)     intro_file="${2:-}"; shift 2 ;;
      --decisions-file) decisions_file="${2:-}"; shift 2 ;;
      *)                die "unknown argument: $1" ;;
    esac
  done
  [ -n "$mode" ] || die "one of --diff or --document is required"
  [ -f "$content_file" ] || die "content file not found: $content_file"
  [ -s "$content_file" ] || die "content file is empty: $content_file — for a diff, did new held/ or shed/ files get 'git add -N' first?"
  [ -z "$readme_file" ] || [ -f "$readme_file" ] || die "readme file not found: $readme_file"
  [ "$mode" = "diff" ] || [ -z "$readme_file" ] || die "--readme only applies with --diff (a --document page already shows the whole file)"
  [ -z "$intro_file" ] || [ -f "$intro_file" ] || die "intro file not found: $intro_file"
  [ -z "$decisions_file" ] || [ -f "$decisions_file" ] || die "decisions file not found: $decisions_file"
}

response_count() {
  local out; out="$(mktemp)"
  wrack_try "$out" page responses "$1" --json
  jq '(.responses? // .) | length' < "$out"
  rm -f "$out"
}

cmd="${1:-}"; shift || die "usage: signoff-page.sh publish|edit|watch|revoke ..."

case "$cmd" in
  publish)
    parse_content_flags "$@"
    html="$(mktemp --suffix=.html)"; trap 'rm -f "$html"' EXIT
    compose "$mode" "$content_file" "$readme_file" "$title" "$intro_file" "$decisions_file" "$html"
    out="$(mktemp)"
    wrack_try "$out" page publish --html "$html" --verbatim --json
    jq '{page_id: (.id // .page_id), url, responses_seen: 0}' < "$out"
    rm -f "$out"
    ;;

  edit)
    page_id="${1:-}"; shift || true
    [ -n "$page_id" ] || die "edit needs a <page-id>"
    parse_content_flags "$@"
    html="$(mktemp --suffix=.html)"; trap 'rm -f "$html"' EXIT
    compose "$mode" "$content_file" "$readme_file" "$title" "$intro_file" "$decisions_file" "$html"
    out="$(mktemp)"
    wrack_try "$out" page edit "$page_id" --html "$html" --verbatim --json
    rm -f "$out"
    # Count responses AFTER the edit lands: anything present by then was
    # submitted before the user could have read the new content, so it is the
    # old round's. Counting before the edit would leave a window where a
    # stale click lands uncounted and the watch misreads it as this round's
    # verdict.
    seen="$(response_count "$page_id")"
    jq -n --argjson seen "$seen" --arg pid "$page_id" \
      '{page_id: $pid, url: null, responses_seen: $seen}'
    ;;

  watch)
    page_id="${1:-}"; shift || true
    [ -n "$page_id" ] || die "watch needs a <page-id>"
    seen="" timeout_secs=570 interval_secs=5
    while [ $# -gt 0 ]; do
      case "$1" in
        --seen)          seen="${2:-}"; shift 2 ;;
        --timeout-secs)  timeout_secs="${2:-}"; shift 2 ;;
        --interval-secs) interval_secs="${2:-}"; shift 2 ;;
        *)               die "unknown argument: $1" ;;
      esac
    done
    [ -n "$seen" ] || die "--seen <n> is required (the responses_seen from publish or edit)"

    deadline=$(( $(date +%s) + timeout_secs ))
    while :; do
      # A failed poll is a blip, not a verdict: keep waiting; the deadline
      # bounds how long. (Persistent unreachability thus surfaces as the
      # timeout, which the caller re-invokes through — still no false exit.)
      count="$(wrack_cmd page responses "$page_id" --json 2>/dev/null | jq '(.responses? // .) | length' 2>/dev/null || echo "$seen")"
      if [ "$count" -gt "$seen" ] 2>/dev/null; then
        # Field values may arrive as scalars or single-element arrays
        # (repeated names collect into arrays); take a scalar view.
        wrack_cmd page responses "$page_id" --json | jq '
          (.responses? // .)
          | sort_by(.submitted_at_unix) | last
          | {
              verdict: (.fields.verdict | if type == "array" then last else . end),
              comments: ((.fields.comments // "") | if type == "array" then join("\n") else . end),
              submitted_at_unix
            }'
        exit 0
      fi
      [ "$(date +%s)" -lt "$deadline" ] || { jq -n '{verdict: "timeout"}'; exit 3; }
      sleep "$interval_secs"
    done
    ;;

  revoke)
    page_id="${1:-}"
    [ -n "$page_id" ] || die "revoke needs a <page-id>"
    out="$(mktemp)"
    wrack_try "$out" page revoke "$page_id"
    cat "$out"; rm -f "$out"
    ;;

  *)
    die "unknown subcommand: $cmd (publish|edit|watch|revoke)"
    ;;
esac
