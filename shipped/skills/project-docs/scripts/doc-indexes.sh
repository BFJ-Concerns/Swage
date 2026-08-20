#!/usr/bin/env bash
# doc-indexes.sh — deterministically (re)generate the documentation indexes
# from the parallel doc trees and their frontmatter, or verify them.
#
#   doc-indexes.sh [ROOT]          regenerate llms.txt, llms-full.txt, and
#                                  the generated region of docs/README.md
#   doc-indexes.sh --check [ROOT]  regenerate to a temp copy, diff against
#                                  the committed files, and enforce the
#                                  mirror rule; nonzero exit on any drift
#
# Trees (relative to ROOT, default "."):
#   agent-docs/  agent set  -> llms.txt (index) + llms-full.txt (corpus)
#   docs/        human set  -> docs/README.md generated region
#
# Frontmatter read (flat "key: value" lines between --- fences only):
#   description  one line for the index (agent pages: only when the title
#                alone is ambiguous; human pages: always)
#   unpaired     why the page deliberately has no mirrored counterpart
#
# Generated regions sit between these markers; text outside them (the
# llms.txt header, the human index preamble) is hand-written and preserved.
# A target that exists without exactly one well-ordered marker pair is an
# error, never an overwrite. Marker text is plain ASCII (no "--", which is
# invalid inside an HTML comment; no characters an editor might normalise).
MARK_START='<!-- doc-index:start (generated region; do not hand-edit. Regenerate with the project-docs skill: doc-indexes.sh) -->'
MARK_END='<!-- doc-index:end -->'

set -eu

check=0
if [ "${1:-}" = "--check" ]; then check=1; shift; fi
root=$(cd "${1:-.}" && pwd)
agent_tree="$root/agent-docs"
human_tree="$root/docs"

fm_field() { # fm_field FILE KEY -> value (empty when absent)
  awk -v key="$2" '
    NR==1 && $0!="---" { exit }
    NR>1 && $0=="---" { exit }
    NR>1 && index($0, key ": ")==1 {
      v=substr($0, length(key)+3)
      gsub(/^["'\'' ]+|["'\'' ]+$/, "", v); print v; exit
    }' "$1"
}

page_title() { # first "# " heading after any frontmatter; falls back to filename
  local t
  t=$(awk '
    NR==1 && $0=="---" { fm=1; next }
    fm && $0=="---" { fm=0; next }
    fm { next }
    /^# / { print substr($0, 3); exit }' "$1")
  [ -n "$t" ] && printf '%s\n' "$t" || basename "$1" .md
}

body_without_fm() {
  awk '
    NR==1 && $0=="---" { fm=1; next }
    fm && $0=="---" { fm=0; skipblank=1; next }
    fm { next }
    skipblank && $0=="" { next }
    { skipblank=0; print }' "$1"
}

section_name() { # first path segment under the tree, prettified; top level -> ""
  case "$1" in
    */*) printf '%s\n' "${1%%/*}" | tr '-' ' ' | awk '{for(i=1;i<=NF;i++) $i=toupper(substr($i,1,1)) substr($i,2)} 1' ;;
    *) printf '' ;;
  esac
}

list_pages() { # repo-relative paths, grouped: top-level pages first, then per subdirectory
  [ -d "$1" ] || return 0
  (cd "$1" && find . -name '*.md' -type f | sed 's|^\./||' \
    | awk -F/ '{ k = (NF==1) ? "0" : "1/" $1; print k "\t" $0 }' \
    | LC_ALL=C sort | cut -f2)
}

render_region() { # render_region TREE LINKPREFIX STYLE(agent|human) EXCLUDE
  local tree=$1 prefix=$2 style=$3 exclude=$4 cursec="" first=1 rel title desc sec
  while IFS= read -r rel; do
    [ -z "$rel" ] && continue
    [ "$rel" = "$exclude" ] && continue
    title=$(page_title "$tree/$rel")
    desc=$(fm_field "$tree/$rel" description)
    sec=$(section_name "$rel")
    if [ "$sec" != "$cursec" ] || [ $first -eq 1 ]; then
      [ $first -eq 1 ] || printf '\n'
      printf '## %s\n\n' "${sec:-General}"
      cursec=$sec; first=0
    fi
    if [ "$style" = human ]; then
      printf -- '- **[%s](%s%s)**%s\n' "$title" "$prefix" "$rel" "${desc:+ — $desc}"
    else
      printf -- '- [%s](%s%s)%s\n' "$title" "$prefix" "$rel" "${desc:+: $desc}"
    fi
  done <<EOF
$(list_pages "$tree")
EOF
}

structure_errors=0
splice() { # splice TARGET REGIONFILE DEFAULTHEADER: replace the marked region, creating TARGET only when absent.
  # Fails closed: an existing target without exactly one well-ordered marker
  # pair is reported and left untouched, never overwritten.
  local target=$1 region=$2 header=$3 tmp s e
  tmp=$(mktemp)
  if [ -f "$target" ]; then
    s=$(grep -cxF "$MARK_START" "$target" || true)
    e=$(grep -cxF "$MARK_END" "$target" || true)
    if [ "$s" -ne 1 ] || [ "$e" -ne 1 ]; then
      echo "markers: $target exists but does not contain exactly one generated-region marker pair (start: $s, end: $e) — add the pair where the generated index belongs, or remove the file; refusing to overwrite" >&2
      structure_errors=$((structure_errors+1)); rm -f "$tmp"; return 0
    fi
    if ! awk -v s="$MARK_START" -v e="$MARK_END" '$0==s{sl=NR} $0==e{el=NR} END{exit !(sl && el && sl<el)}' "$target"; then
      echo "markers: $target has its end marker before its start marker — fix the pair; refusing to overwrite" >&2
      structure_errors=$((structure_errors+1)); rm -f "$tmp"; return 0
    fi
    awk -v s="$MARK_START" -v e="$MARK_END" -v rf="$region" '
      $0==s { print; while ((getline line < rf) > 0) print line; skip=1; next }
      $0==e { skip=0 }
      !skip { print }' "$target" > "$tmp"
  else
    { printf '%s\n\n%s\n' "$header" "$MARK_START"; cat "$region"; printf '%s\n' "$MARK_END"; } > "$tmp"
  fi
  printf '%s\n' "$tmp"
}

# ---- mirror rule: every page has a counterpart or an unpaired declaration
mirror_errors=0
if [ -d "$agent_tree" ]; then
  while IFS= read -r rel; do
    [ -z "$rel" ] && continue
    if [ ! -f "$human_tree/$rel" ] && [ -z "$(fm_field "$agent_tree/$rel" unpaired)" ]; then
      echo "mirror: agent-docs/$rel has no docs/$rel counterpart and no 'unpaired' declaration" >&2
      mirror_errors=$((mirror_errors+1))
    fi
  done <<EOF
$(list_pages "$agent_tree")
EOF
  while IFS= read -r rel; do
    [ -z "$rel" ] && continue
    [ "$rel" = "README.md" ] && continue
    if [ ! -f "$agent_tree/$rel" ] && [ -z "$(fm_field "$human_tree/$rel" unpaired)" ]; then
      echo "mirror: docs/$rel has no agent-docs/$rel counterpart and no 'unpaired' declaration" >&2
      mirror_errors=$((mirror_errors+1))
    fi
  done <<EOF
$(list_pages "$human_tree")
EOF
fi

# ---- frontmatter validation: mandatory human descriptions; no block scalars
fm_errors=0
validate_pages() { # validate_pages TREE LABEL REQUIRE_DESC EXCLUDE
  local tree=$1 label=$2 require=$3 exclude=$4 rel desc unp
  [ -d "$tree" ] || return 0
  while IFS= read -r rel; do
    [ -z "$rel" ] && continue
    [ "$rel" = "$exclude" ] && continue
    desc=$(fm_field "$tree/$rel" description)
    unp=$(fm_field "$tree/$rel" unpaired)
    for v in "$desc" "$unp"; do
      case "$v" in
        '>'*|'|'*)
          echo "frontmatter: $label/$rel uses a YAML block scalar; index fields must be single-line values" >&2
          fm_errors=$((fm_errors+1)) ;;
      esac
    done
    if [ "$require" = yes ] && [ -z "$desc" ]; then
      echo "frontmatter: $label/$rel has no description — every human page carries one for the index" >&2
      fm_errors=$((fm_errors+1))
    fi
  done <<EOF
$(list_pages "$tree")
EOF
}
validate_pages "$human_tree" docs yes "README.md"
validate_pages "$agent_tree" agent-docs no ""

# An index left behind by a removed agent tree is drift, not a skip.
if [ ! -d "$agent_tree" ]; then
  for f in "$root/llms.txt" "$root/llms-full.txt"; do
    [ -f "$f" ] && { echo "orphan: $(basename "$f") exists but agent-docs/ does not — remove the index or restore the tree" >&2; mirror_errors=$((mirror_errors+1)); }
  done
fi

# ---- build the generated artefacts into a temp area
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -d "$agent_tree" ]; then
  render_region "$agent_tree" "agent-docs/" agent "" > "$work/llms.region"
  new=$(splice "$root/llms.txt" "$work/llms.region" "# $(basename "$root")

> Agent-facing documentation index. Each linked page is the terse, canonical rendering of its subject; llms-full.txt carries the whole set in one file.")
  if [ -n "$new" ]; then mv "$new" "$work/llms.txt"; else structure_errors=$((structure_errors+1)); fi

  {
    printf '<!-- generated from agent-docs/ by the project-docs skill (doc-indexes.sh); do not edit -->\n'
    while IFS= read -r rel; do
      [ -z "$rel" ] && continue
      printf '\n\n<!-- source: agent-docs/%s -->\n\n' "$rel"
      body_without_fm "$agent_tree/$rel"
    done <<EOF
$(list_pages "$agent_tree")
EOF
  } > "$work/llms-full.txt"
fi

if [ -d "$human_tree" ]; then
  render_region "$human_tree" "" human "README.md" > "$work/readme.region"
  new=$(splice "$human_tree/README.md" "$work/readme.region" "# Documentation")
  if [ -n "$new" ]; then mv "$new" "$work/docs-README.md"; else structure_errors=$((structure_errors+1)); fi
fi

# ---- write or check
status=0
emit() { # emit WORKFILE TARGET
  [ -f "$1" ] || return 0
  if [ $check -eq 1 ]; then
    if ! diff -u "$2" "$1" >/dev/null 2>&1; then
      echo "stale: $2 does not match regeneration (run scripts/doc-indexes.sh)" >&2
      status=1
    fi
  else
    cp "$1" "$2"
  fi
}
emit "$work/llms.txt" "$root/llms.txt"
emit "$work/llms-full.txt" "$root/llms-full.txt"
emit "$work/docs-README.md" "$human_tree/README.md"

[ $((mirror_errors + structure_errors + fm_errors)) -gt 0 ] && status=1
exit $status
