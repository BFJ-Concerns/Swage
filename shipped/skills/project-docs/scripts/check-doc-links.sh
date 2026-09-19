#!/usr/bin/env bash
# Verify that relative Markdown links resolve to real files.
#
# Usage: check-doc-links.sh [file-or-dir ...]    (default: current directory)
#
# Prints one line per broken or absolute link; exits 1 if any were found, and
# 2 if a file could not be scanned at all (the extraction pipeline failed), so
# a checker that cannot read a document never reports it clean. Fenced code
# blocks are excluded. Anchors (#section) are not validated — only the file
# part of a link is checked.
set -uo pipefail

broken=0
unscanned=0

# Print the inline link targets ([text](target)) of one file, fenced code
# blocks removed. Fails when any stage of the extraction fails.
extract_links() {
  local file=$1
  awk '/^[[:space:]]*```/{fence=!fence; next} !fence' "$file" \
    | grep -oE '\]\([^)]+\)' \
    | sed -E 's/^\]\(//; s/\)$//'
  local status=("${PIPESTATUS[@]}")
  # grep exits 1 for "no matches", which is a clean result, not a failure.
  [ "${status[0]}" -eq 0 ] && [ "${status[1]}" -le 1 ] && [ "${status[2]}" -eq 0 ]
}

check_file() {
  local file=$1 dir link path links
  dir=$(dirname "$file")
  if ! links=$(extract_links "$file"); then
    printf 'could not scan: %s\n' "$file" >&2
    unscanned=1
    return
  fi
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
  done <<< "$links"
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
    unscanned=1
  fi
done

if [ "$unscanned" -ne 0 ]; then
  exit 2
fi
exit "$broken"
