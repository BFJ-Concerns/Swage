#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

blocked_pathspecs=(
  ".codex/loop"
  ".swage/materials"
  ".swage/tool-output"
)

tracked_local_state=()

for pathspec in "${blocked_pathspecs[@]}"; do
  while IFS= read -r tracked_path; do
    tracked_local_state+=("$tracked_path")
  done < <(git ls-files -- "$pathspec")
done

if ((${#tracked_local_state[@]} > 0)); then
  {
    echo "Tracked local runtime state is not allowed."
    echo
    printf '  %s\n' "${tracked_local_state[@]}"
    echo
    echo "Remove these paths from the index and keep them ignored:"
    echo "  git rm --cached -r .codex/loop .swage/materials .swage/tool-output"
  } >&2
  exit 1
fi
