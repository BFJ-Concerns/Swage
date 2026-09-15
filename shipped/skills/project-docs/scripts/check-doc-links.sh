#!/usr/bin/env bash
# Verify that relative Markdown links resolve to real files.
#
# Usage: check-doc-links.sh [file-or-dir ...]    (default: current directory)
#
# Prints one line per broken or absolute link; exits 1 if any were found.
# Fenced code blocks are excluded. Anchors (#section) are not validated —
# only the file part of a link is checked.
set -uo pipefail

broken=0

check_file() {
  local file=$1 dir link path
  dir=$(dirname "$file")
  # Strip fenced code blocks, then pull out inline link targets: [text](target)
  while IFS= read -r link; do
    case "$link" in
      http://*|https://*|mailto:*|'#'*|'') continue ;;
    esac
    path=${link%%#*}    # drop the anchor
    path=${path%% *}    # drop an optional link title
    [ -n "$path" ] || continue
    if [ "${path#/}" != "$path" ]; then
      printf 'absolute link (use relative): %s -> %s\n' "$file" "$link"
      broken=1
    elif [ ! -e "$dir/$path" ]; then
      printf 'broken link: %s -> %s\n' "$file" "$link"
      broken=1
    fi
  done < <(awk '/^[[:topic:]]*```/{fence=!fence; next} !fence' "$file" \
             | grep -oE '\]\([^)]+\)' \
             | sed -E 's/^\]\(//; s/\)$//')
}

for target in "${@:-.}"; do
  if [ -f "$target" ]; then
    check_file "$target"
  elif [ -d "$target" ]; then
    while IFS= read -r file; do
      check_file "$file"
    done < <(find "$target" -name '*.md' -not -path '*/.git/*' -not -path '*/node_modules/*' | sort)
  else
    printf 'no such file or directory: %s\n' "$target" >&2
    broken=1
  fi
done

exit "$broken"
