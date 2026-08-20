#!/usr/bin/env bash
#
# create-annexe.sh — bring a project's annexe into existence, idempotently.
#
# An annexe is the planning sibling of a code repo `foo`: a repo named
# `foo-Annexe` on the same Forgejo account, holding intent (README.md, the
# commission), ideas, issues, and orchestrator scratch. This script is the
# single front door for "make sure the annexe exists and is complete". It is
# safe to re-run: it creates only what is missing, so it doubles as the
# existence check, the first-time scaffolder, and the repair for a half-built
# annexe. It never rewrites a file that already exists: refreshing scaffold
# content that has drifted from current conventions is a judgement call the
# commission update conversation makes and surfaces to the user, not a blind
# overwrite this script could safely make on its own.
#
# What it does NOT do: write README.md. The README is the commission — the
# single-writer contract authored through the commissioning conversation and
# published only on sign-off. This script lays down everything around it and
# bootstraps the remote repo (push-to-create), but the commission itself never
# reaches the remote unsigned. A freshly scaffolded annexe with no README is
# one whose commission is still being written.
#
# Decision tree:
#   * local repo present            -> repair in place
#   * absent locally, present remote -> clone it (the "update missing its
#                                       clone" case), then repair — never
#                                       clobber an existing annexe
#   * absent both                    -> init, scaffold, commit, push-to-create
#
# Usage:
#   create-annexe.sh --annexe-dir <path> --remote <host-alias:account> \
#       [--code-dir <path>] [--merge-policy direct-merge|pr] \
#       [--push-policy push-on-land|hold-for-user]
#
#   --annexe-dir   Where the annexe lives (e.g. ../foo-Annexe). Required.
#   --remote       Forgejo account spec, e.g. forge:my-account (an SSH host
#                  alias and the account that owns the code repo). The
#                  push URL is "<remote>/<annexe-name>.git". Required.
#   --code-dir     The code repo whose CLAUDE.local.md pointer to write. If
#                  omitted, derived as the sibling without the -Annexe suffix; the
#                  pointer is written only when that directory is a git repo
#                  (greenfield may have no code repo yet).
#   --merge-policy Written into config.toml when known. Omit to leave a
#                  commented placeholder the conversation fills once settled.
#   --push-policy  Written into config.toml when known. Omit to leave a
#                  commented placeholder the conversation fills once settled.

set -euo pipefail

# --- Argument parsing -------------------------------------------------------

annexe_dir=""
remote=""
code_dir=""
merge_policy=""
push_policy=""

die() { printf 'create-annexe: %s\n' "$1" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --annexe-dir)   annexe_dir="${2:-}"; shift 2 ;;
    --remote)       remote="${2:-}"; shift 2 ;;
    --code-dir)     code_dir="${2:-}"; shift 2 ;;
    --merge-policy) merge_policy="${2:-}"; shift 2 ;;
    --push-policy)  push_policy="${2:-}"; shift 2 ;;
    -h|--help)      { grep '^#' "$0" | grep -v '^#!' | sed 's/^# \{0,1\}//'; } || true; exit 0 ;;
    *)              die "unknown argument: $1" ;;
  esac
done

[ -n "$annexe_dir" ] || die "--annexe-dir is required"
[ -n "$remote" ]     || die "--remote is required (e.g. forge:my-account)"

case "$merge_policy" in
  ""|direct-merge|pr) ;;
  *) die "--merge-policy must be 'direct-merge' or 'pr', got '$merge_policy'" ;;
esac

case "$push_policy" in
  ""|push-on-land|hold-for-user) ;;
  *) die "--push-policy must be 'push-on-land' or 'hold-for-user', got '$push_policy'" ;;
esac

# Resolve the annexe path to absolute via its parent, which must already exist
# (the annexe is created as a sibling of the code repo, not the parent tree).
parent="$(cd "$(dirname "$annexe_dir")" 2>/dev/null && pwd)" \
  || die "parent of --annexe-dir does not exist: $(dirname "$annexe_dir")"
name="$(basename "$annexe_dir")"
annexe_dir="$parent/$name"
push_url="$remote/$name.git"

# Project name and code repo. The annexe name is conventionally <project>-Annexe;
# derive the code repo from it only when that convention holds, so an oddly
# named annexe doesn't silently point CLAUDE.local.md at itself.
project="${name%-Annexe}"
if [ -z "$code_dir" ] && [ "$project" != "$name" ]; then
  code_dir="$parent/$project"
fi

# Normalise an explicit --code-dir to absolute so it resolves the same way as
# the derived one — a relative path would otherwise bind to the caller's CWD
# rather than the annexe's parent.
if [ -n "$code_dir" ] && [ -d "$code_dir" ]; then
  code_dir="$(cd "$code_dir" && pwd)"
fi

# --- Establish the local repo ----------------------------------------------

# Decide where we are: a local clone already present, an annexe already on the
# remote, or a true green field. The remote probe must not mistake an
# unreachable host for an absent repo — concluding "absent" wrongly would
# scaffold and attempt a push-to-create. (Git's non-fast-forward protection
# still refuses to overwrite a real annexe, but a blind scaffold on a network
# blip is a confusing dead end, so we'd rather refuse than guess.)
mode=""
if [ -d "$annexe_dir/.git" ]; then
  mode="repair"
  echo "create-annexe: local annexe present at $annexe_dir — repairing."
else
  if probe_err="$(git ls-remote "$push_url" 2>&1 >/dev/null)"; then
    remote_exists=yes
  else
    remote_exists=no
    # A transport failure (unreachable host, dropped connection) is not the same
    # as "repo absent" — both exit 128. Refuse on the unambiguous transport
    # errors rather than blindly creating; an auth/not-found message falls
    # through to create, where push-to-create (and git's non-ff guard) decide.
    case "$probe_err" in
      *"Could not resolve hostname"*|*"Connection refused"*|*"Connection timed out"*\
      |*"Network is unreachable"*|*"Operation timed out"*|*"Connection closed"*)
        die "cannot reach the remote ($push_url) to check whether the annexe exists: ${probe_err}. Refusing to scaffold blindly — fix connectivity and re-run." ;;
    esac
  fi

  if [ "$remote_exists" = yes ]; then
    # The annexe exists on Forgejo but isn't checked out here: an update
    # conversation missing its clone, not a green field. Clone rather than init.
    mode="repair"
    # git clone fatals into a non-empty directory. A stray half-built dir with
    # no .git would otherwise abort cryptically — refuse with guidance instead.
    if [ -e "$annexe_dir" ] && [ -n "$(ls -A "$annexe_dir" 2>/dev/null)" ]; then
      die "an annexe exists on the remote, but $annexe_dir already holds files and is not a git repo. Move or remove that directory, then re-run to clone the real annexe — refusing to clone over it."
    fi
    echo "create-annexe: remote annexe exists — cloning $push_url."
    git clone "$push_url" "$annexe_dir"
  else
    mode="create"
    echo "create-annexe: no annexe found — initialising $annexe_dir."
    mkdir -p "$annexe_dir"
    git -C "$annexe_dir" init -b main >/dev/null
  fi
fi

git_a() { git -C "$annexe_dir" "$@"; }

# Track only files this scaffolder created or repaired during this invocation.
# The annexe may already hold an unsigned README draft or unrelated local work;
# those are not ours to stage under a scaffolding commit.
scaffold_paths=()
track_scaffold_path() { scaffold_paths+=("$1"); }

# --- Scaffold missing parts (idempotent: create only what is absent) --------

# Orientation for cold-landing agents. AGENTS.md is the source of truth; CLAUDE.md
# imports it so Claude Code and the AGENTS.md-reading tools share one file.
if [ ! -e "$annexe_dir/AGENTS.md" ]; then
  cat > "$annexe_dir/AGENTS.md" <<EOF
# $name

Planning annexe for the \`$project\` project — intent, ideas, and issues, kept
out of the code repo so the code stays code.

- \`README.md\` — the commission: this project's contract (its why, the
  destination, constraints, decisions). Single-writer; it changes only through a
  commissioning conversation.
- \`IDEAS.md\` — the user's zero-ceremony inbox, their pen alone: ideas,
  questions, things to look into later. Processed only by a commissioning
  conversation.
- \`ISSUES.md\` — the shared find-ingest log: bugs and tangential finds noted in
  passing, appended by agents and the user alike. Triaged elsewhere, not acted on
  in place — destination finds go to the README, decided work to \`TASKS.md\`.
- \`TASKS.md\` — decided-journey queue: already-triaged work for the build but
  not the contract. Written by the user, the build phases, and a commissioning
  conversation, which tags each item's kind; \`(journey)\` items are drained by
  assembly runs, \`(structural)\` items by maestro phases, untagged items await
  triage. Runs mark progress on the item's own line, never in prose beneath it.
- \`FRICTION.md\` — friction ledger: dated observations of a code area resisting
  change, appended by agents whenever a change fought back harder than the work
  warranted. Reviewed for recurrence at commission updates; entries persist as
  evidence rather than being drained.
- \`held/\` — held intent: features planned to full contract rigour but
  deliberately withheld from building. Like the README, it changes only
  through a commissioning conversation.
- \`shed/\` — append-only archive of detail shed from the README; historical
  record, not current intent. The README wins.
- \`config.toml\` — runtime knobs (e.g. \`merge_policy\`, \`push_policy\`).
  Plain config, not contract.

The repository root is contract; any orchestrator subdirectory is scratch.
Commit annexe changes as they happen, with plain descriptive messages.
EOF
  track_scaffold_path "AGENTS.md"
fi

# CLAUDE.md just imports AGENTS.md, so Claude Code loads the same orientation. The
# import is portable (a symlink would need Administrator rights on Windows). Left
# alone if it already exists as a real file — we don't clobber existing content.
if [ ! -e "$annexe_dir/CLAUDE.md" ]; then
  printf '@AGENTS.md\n' > "$annexe_dir/CLAUDE.md"
  track_scaffold_path "CLAUDE.md"
fi

# config.toml. Each settled policy is written live; an unsettled one gets a
# commented placeholder the conversation uncomments once it is decided.
if [ ! -e "$annexe_dir/config.toml" ]; then
  {
    cat <<'EOF'
# Annexe runtime configuration. Plain config, editable without a commissioning
# conversation — read by the orchestrator and any agent or script.

EOF
    if [ -n "$merge_policy" ]; then
      printf 'merge_policy = "%s"\n' "$merge_policy"
    else
      cat <<'EOF'
# merge_policy governs how a finished unit of work lands:
#   "direct-merge" — workers merge to main themselves (young projects; main is
#                    cheap to fix)
#   "pr"           — workers push branches and open PRs for review (load-bearing
#                    main)
# Settle it during commissioning, then uncomment:
# merge_policy = "direct-merge"
EOF
    fi
    echo
    if [ -n "$push_policy" ]; then
      printf 'push_policy = "%s"\n' "$push_policy"
    else
      cat <<'EOF'
# push_policy governs whether agents push code-repo work to origin unprompted:
#   "push-on-land"  — pushing finished, committed work is part of the job
#   "hold-for-user" — commit locally; the push waits for the user's say-so
# The annexe itself is exempt: annexe commits are always pushed as they land.
# Settle it during commissioning, then uncomment:
# push_policy = "push-on-land"
EOF
    fi
  } > "$annexe_dir/config.toml"
  track_scaffold_path "config.toml"
fi

# Working-file stubs. Empty but for a one-line header, so a cold reader knows
# each file's role before anything is written into it.
if [ ! -e "$annexe_dir/IDEAS.md" ]; then
  printf '# Ideas\n\nThe user'\''s zero-ceremony inbox — ideas, questions, things to look into later. Their pen alone; processed only by a commissioning conversation.\n' \
    > "$annexe_dir/IDEAS.md"
  track_scaffold_path "IDEAS.md"
fi
if [ ! -e "$annexe_dir/ISSUES.md" ]; then
  printf '# Issues\n\nShared find-ingest log: bugs and tangential finds noted in passing, appended by agents and the user alike. Triaged elsewhere, never acted on in place.\n' \
    > "$annexe_dir/ISSUES.md"
  track_scaffold_path "ISSUES.md"
fi
if [ ! -e "$annexe_dir/TASKS.md" ]; then
  printf '# Tasks\n\nDecided build queue, not contract. One checkbox item per line, stamped (added <date>) when it enters — the age "oldest" resolves to; commission triage tags it (journey) — assembly runs drain it — or (structural) — maestro absorbs it at phase boundaries; untagged awaits triage. Runs record progress on the item'"'"'s own line: (blocked on <blocker> since <date>) when a named dependency stops it, (→ <slug>) on conversion into the build ledger, a fixes-batch obligation included; an item a run cannot take yet stays open, unmarked. The line below records the commission pass that last re-judged every item.\n\nLast triage: none\n' \
    > "$annexe_dir/TASKS.md"
  track_scaffold_path "TASKS.md"
fi
if [ ! -e "$annexe_dir/FRICTION.md" ]; then
  printf '# Friction\n\nFriction ledger: dated observations of a code area resisting change (area, symptom, expected-versus-actual), appended by agents whenever a change fought back harder than the work warranted. A single entry needs no confidence — recurrence is what builds a case — so log the lead rather than judging it. Reviewed for recurrence at commission updates; entries persist as evidence rather than being drained.\n' \
    > "$annexe_dir/FRICTION.md"
  track_scaffold_path "FRICTION.md"
fi

# held/ and shed/ start empty; a .gitkeep makes each otherwise-untracked
# directory survive the commit. Their files (one per held or shed feature) are
# written later, by the skill.
for d in held shed; do
  mkdir -p "$annexe_dir/$d"
  if [ ! -e "$annexe_dir/$d/.gitkeep" ]; then
    : > "$annexe_dir/$d/.gitkeep"
    track_scaffold_path "$d/.gitkeep"
  fi
done

# --- Code-repo pointer ------------------------------------------------------

# An uncommitted discovery pointer in the code repo: a single line saying intent
# lives in the sibling annexe. Written only when a code repo exists (greenfield
# may have none yet — then the pointer is build-time business). It goes in
# CLAUDE.local.md, which Claude reads and the *.local gitignore convention keeps
# unstaged. Codex has no equivalent uncommitted per-project file — a project-root
# AGENTS.override.md would shadow the repo's committed AGENTS.md rather than add to
# it — so the pointer is Claude-side only.
if [ -n "$code_dir" ] && [ -d "$code_dir/.git" ]; then
  pf="CLAUDE.local.md"
  if [ ! -e "$code_dir/$pf" ]; then
    printf 'Planning, intent, and issues for this project live in the sibling annexe repo: `../%s` (its `README.md` is the commission — this project'\''s contract).\n' \
      "$name" > "$code_dir/$pf"
    echo "create-annexe: wrote $code_dir/$pf (uncommitted pointer)."
  fi
else
  # Say so explicitly: a silent skip can't be told apart from a naming mismatch
  # that left code_dir unset. Greenfield with no code repo yet is the usual case.
  echo "create-annexe: no code repo found${code_dir:+ at $code_dir} — skipped local pointer (greenfield, or pass --code-dir)."
fi

# --- Commit and push --------------------------------------------------------

if [ "${#scaffold_paths[@]}" -gt 0 ]; then
  git_a add -- "${scaffold_paths[@]}"
fi
if [ "${#scaffold_paths[@]}" -eq 0 ] || git_a diff --cached --quiet -- "${scaffold_paths[@]}"; then
  echo "create-annexe: nothing to commit — annexe already complete."
else
  if [ "$mode" = "create" ]; then
    git_a commit -q -m "Initialise annexe scaffolding" -- "${scaffold_paths[@]}"
  else
    git_a commit -q -m "Restore missing annexe scaffolding" -- "${scaffold_paths[@]}"
  fi
  echo "create-annexe: committed scaffolding."
fi

# Push-to-create: the instance has push-to-create enabled, so the first push to
# a not-yet-existent Forgejo repo creates it, private by default. A failure here
# leaves the work safely committed locally — re-running just retries the push.
#
# Push to the intended URL explicitly, never to whatever 'origin' happens to be
# (a pre-existing local repo could carry an origin pointing elsewhere). Keep an
# 'origin' remote for convenience when it is absent or already correct.
current_origin="$(git_a remote get-url origin 2>/dev/null || true)"
if [ -z "$current_origin" ]; then
  git_a remote add origin "$push_url"
elif [ "$current_origin" != "$push_url" ]; then
  echo "create-annexe: note — 'origin' is $current_origin, not the annexe URL; pushing to $push_url explicitly."
fi
echo "create-annexe: pushing to $push_url"
git_a push "$push_url" main

echo "create-annexe: done ($mode). The README/commission is authored separately and pushed only on sign-off."
