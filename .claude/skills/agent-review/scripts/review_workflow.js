export const meta = {
  name: 'agent-review',
  description: 'Fan out one review agent per .review/ brief and collate their findings',
  phases: [
    { title: 'Review', detail: 'one clean-context agent per shard (large briefs split across several)' },
  ],
}

// args is { plan } — the discovery plan from plan_review.py, the only payload the
// skill relays. Workflow scripts cannot read files, so whatever the script needs
// from disk must arrive through args; but the reviewer *prompt* deliberately does
// NOT. It is static (prompts/reviewer-method.md) and several KB of backtick- and
// brace-heavy prose — relaying that verbatim through args proved unreliable, and
// a single garbled character crashed the whole run at the JSON.parse below (0
// agents, a few milliseconds, an opaque "Unable to parse JSON string"). Instead
// the plan carries the template's absolute path and each reviewer agent reads it
// itself, the same way it already reads its own brief file. Only compact,
// structured data rides args now — including each brief's shard descriptors,
// from which the reviewers materialise their own file slices (see buildPrompt).
//
// Defensive parse: the Workflow tool usually delivers args as a parsed object,
// but some harness builds hand over the raw JSON string. Normalise to an object,
// and if that string is malformed, fail with a message that names the cause
// rather than the bare engine error.
let input
try {
  input = typeof args === 'string' ? JSON.parse(args) : args
} catch (err) {
  const text = String(args)
  throw new Error(
    `agent-review: args was a string but not valid JSON (length ${text.length}), ` +
      `so the plan could not be read and no reviewers ran. First 200 chars: ` +
      text.slice(0, 200),
  )
}
const plan = input && input.plan
if (!plan) {
  throw new Error(
    `agent-review: args did not contain a "plan". Expected { plan }, got keys: ` +
      (input ? Object.keys(input).join(', ') || '(none)' : String(input)),
  )
}
const root = plan.repo_root
const mode = plan.mode
// Absolute path to the shared reviewer method file (prompts/reviewer-method.md),
// supplied by plan_review.py. Each reviewer reads it rather than receiving its
// text through args — see the note above.
const templatePath = plan.template_path
if (!templatePath) {
  throw new Error(
    'agent-review: plan is missing "template_path". Re-run plan_review.py so it ' +
      'can supply the reviewer method file path.',
  )
}

// Structured output contract for every reviewer. Validation happens at the
// tool-call layer, so an agent that returns the wrong shape is made to retry —
// this is what replaces the old "parse a JSON block, drop malformed" handling.
const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['brief', 'findings'],
  properties: {
    brief: { type: 'string' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['file', 'priority', 'title', 'message', 'code_quote'],
        properties: {
          file: { type: 'string' },
          line: { type: ['integer', 'null'] },
          side: { type: 'string', enum: ['RIGHT', 'LEFT'] },
          priority: { type: 'string', enum: ['P0', 'P1', 'P2', 'P3'] },
          title: { type: 'string' },
          message: { type: 'string' },
          // The verbatim line(s) the finding flags, copied from the file. Required:
          // it is the evidence the finding is grounded in code that actually
          // exists. A reviewer that cannot quote the offending code is told to
          // drop the finding, so this field is what stops fabricated findings —
          // nonexistent functions, wrong line numbers — from reaching a PR.
          code_quote: { type: 'string' },
          suggestion: { type: 'string' },
          // Diff mode only: true when the reviewer incidentally noticed an
          // existing violation the change did not introduce. These are reported
          // to the user in chat and never posted to the PR.
          preexisting: { type: 'boolean' },
        },
      },
    },
    notes: { type: 'string' },
  },
}

function scopeDescription(brief) {
  return brief.scope
    ? `Files under \`${brief.scope}/\`. Only review code within this subtree.`
    : 'The entire repository.'
}

function fileListFrom(files) {
  return (files || []).map((f) => `- ${f}`).join('\n')
}

// Slice commands for a sharded reviewer. The brief's file set is split across
// `shard.total` reviewers; this one materialises exactly its 1-based line range.
// `grep -v` drops the .review/ briefs (matching plan_review.py's `reviewable`),
// `LC_ALL=C sort` fixes one stable order so the shards' ranges tile the set with
// no gap or overlap, and `sed -n` cuts this reviewer's range. The enumerated set
// matches what plan_review.py counted, so the ranges line up.
function lsFilesSlice(brief, shard) {
  const base = brief.scope ? `git ls-files -- ${brief.scope}` : 'git ls-files'
  return `${base} | grep -v '^\\.review/' | LC_ALL=C sort | sed -n '${shard.start},${shard.end}p'`
}

function diffNameSlice(brief, shard) {
  const scoped = brief.scope ? ` -- ${brief.scope}` : ''
  return `git diff --name-only --diff-filter=ACMR ${plan.base_ref}...HEAD${scoped} | grep -v '^\\.review/' | LC_ALL=C sort | sed -n '${shard.start},${shard.end}p'`
}

// The "which files is THIS reviewer responsible for" clause for a sharded brief:
// it establishes the slice and the hard boundary (don't widen, don't narrow).
// The surrounding framing says *how* to review those files — full audit vs diff.
function shardedSliceClause(shard, command) {
  return [
    `This review is split across ${shard.total} reviewers to keep each one's load manageable; you are reviewer ${shard.index} of ${shard.total}. The files you are responsible for are exactly those produced by:`,
    `    ${command}`,
    `Run that command and handle only the files it lists — the other reviewers cover the rest of the scope, so do not widen beyond your slice, and do not narrow within it.`,
  ].join('\n\n')
}

// How a full-extent reviewer enumerates its own scope. We hand it a deterministic
// git command rather than a precomputed list (see plan_review.py): the same files,
// without the args bloat of serialising every path through the model's context. A
// repo-wide brief lists everything; a scoped brief filters to its subtree.
function enumerateCommand(brief) {
  return brief.scope ? `\`git ls-files -- ${brief.scope}\`` : '`git ls-files`'
}

function scopePhrase(brief) {
  return brief.scope ? `\`${brief.scope}/\`` : 'the repository'
}

// What this reviewer covers and how, given its brief and its shard. A brief with
// a small file set (or a whole-tree brief) has a single shard; a large per-file
// brief has several, and each reviewer here is handed exactly one slice. The
// per-branch framing (full sweep, full-extent classify, diff review) is unchanged
// from the single-agent design — sharding only swaps the file-selection clause.
function targetDescription(brief, shard) {
  const sharded = shard.total > 1

  // A --full run audits the whole scope and reports to chat, so no PR
  // classification is needed — just audit every in-scope file.
  if (mode === 'full') {
    const coverage = sharded
      ? [
          shardedSliceClause(shard, lsFilesSlice(brief, shard)),
          `Read every file in your slice that is relevant to your brief — do not sample. Files plainly unrelated to your concern (assets, fonts, lockfiles, generated output) need no review.`,
        ].join('\n\n')
      : `List the tracked files in scope with ${enumerateCommand(brief)}, then review every file relevant to your brief — read them, do not sample. Files plainly unrelated to your concern (assets, fonts, lockfiles, generated output) need no review; use judgement, but do not skip code that could violate the brief.`
    return [
      `This is a full conformance sweep, not a diff review. Audit ${scopePhrase(brief)} against your brief.`,
      coverage,
      `Your brief may be written around what "changed on the branch" or was "added or modified" — ignore that framing here. In a full sweep there is no branch to diff against: judge the files as they stand. Do not run \`git diff\` or otherwise narrow to changed lines.`,
    ].join('\n\n')
  }

  // Diff run, full-extent brief: audit the whole scope, but still classify each
  // finding against the branch so change-caused ones reach the PR and the rest
  // go to the user separately. The brief's own `extent: full` puts it here.
  if (brief.extent === 'full') {
    const base = plan.base_ref
    const coverage = sharded
      ? [
          `Your brief audits its whole scope, not only what changed.`,
          shardedSliceClause(shard, lsFilesSlice(brief, shard)),
          `Read every file in your slice that is relevant to your brief — do not sample. Files plainly unrelated to your concern (assets, fonts, lockfiles, generated output) need no review.`,
        ].join('\n\n')
      : `Your brief audits its whole scope, not only what changed. List the tracked files in ${scopePhrase(brief)} with ${enumerateCommand(brief)}, then review every file relevant to your brief — read them, do not sample. Files plainly unrelated to your concern (assets, fonts, lockfiles, generated output) need no review.`
    return [
      coverage,
      `This branch is being compared against the base \`${base}\`. Classify each violation you find: if it is a product of this branch's changes — it did not exist before the branch, or the branch's edits newly caused it (inspect \`git diff ${base}...HEAD\` to judge) — set \`preexisting: false\`; it belongs on the PR. If it exists independently of this branch, set \`preexisting: true\`; it is reported to the user separately, not on the PR. Report both kinds.`,
    ].join('\n\n')
  }

  // Diff run, diff-extent brief (the common case): review only what changed, and
  // judge the violations the branch introduces or touches. Reviewers get the base
  // ref so findings land on lines that exist in the diff.
  const base = plan.base_ref
  const coverage = sharded
    ? shardedSliceClause(shard, diffNameSlice(brief, shard))
    : `The changed files within your scope are:\n${fileListFrom(shard.files)}`
  return [
    `Review the changes introduced on the current branch against your brief. They are being compared against the base \`${base}\`.`,
    `Inspect the actual diff for each changed file in your scope — for example \`git diff ${base}...HEAD -- <file>\` — and judge the violations the branch introduces or touches. Set \`preexisting: false\` (or omit it) on these.`,
    `Do not go hunting for pre-existing problems. But if, while reading the diff and its surrounding context, you happen to notice an existing violation of your brief on lines the branch did not change, include it as a finding with \`preexisting: true\`. These are surfaced to the user separately and never posted to the PR, so flag them rather than staying silent.`,
    coverage,
  ].join('\n\n')
}

// The per-brief prompt. It carries only the small, brief-specific parts — which
// brief, where it lives, its scope and review target — and points the agent at
// the shared reviewer method file for the working discipline and the exact
// findings format. That file is static and identical for every reviewer, so the
// agent reads it directly rather than having it relayed through args (see top).
function buildPrompt(brief, shard) {
  return [
    `You are a code reviewer with a single, narrow mandate. Review the code described below **only** against the brief given to you. Ignore everything outside the brief — other reviewers cover other concerns, and findings outside your mandate are noise.`,
    `## Your brief: ${brief.name}`,
    `Read the brief file at \`${root}/${brief.path}\` (the repo root is \`${root}\`). It is plain prose describing a concern and what good looks like. The brief defines the *concern* you judge — review for that and nothing else. It does **not** decide *which* code or *how much* of it you cover: the "Scope" and "What to review" sections below govern that, and they take precedence over any incidental framing in the brief. For instance, a brief phrased around what "changed on the branch" still gets a whole-scope audit when this run is a full sweep — follow the instructions below, not the brief's wording, on extent.`,
    `## Scope\n\n${scopeDescription(brief)}`,
    `## What to review\n\n${targetDescription(brief, shard)}`,
    `## How to work and report\n\nRead and follow \`${templatePath}\` — the working method every reviewer on this panel uses and the exact structure for the findings you return. Set \`brief\` to "${brief.name}" in your structured output.`,
  ].join('\n\n')
}

// Briefs with a skip_reason (in diff mode: no changed files in scope) don't run.
const active = plan.briefs.filter((b) => !b.skip_reason)
const skipped = plan.briefs
  .filter((b) => b.skip_reason)
  .map((b) => ({ name: b.name, scope: b.scope, reason: b.skip_reason }))

// Flatten briefs into (brief, shard) tasks — the unit of fan-out is the shard,
// not the brief. A small or whole-tree brief contributes one shard; a large
// per-file brief contributes several, each a slice of its scope. plan_review.py
// always emits `shards` for an active brief, so a missing list means a stale
// planner — fail clearly rather than silently reviewing the wrong set.
const tasks = []
for (const brief of active) {
  if (!brief.shards || !brief.shards.length) {
    throw new Error(
      `agent-review: brief "${brief.name}" has no shards in the plan — re-run ` +
        `plan_review.py (the planner and this workflow must be the same version).`,
    )
  }
  for (const shard of brief.shards) tasks.push({ brief, shard })
}

phase('Review')
log(
  `Reviewing ${active.length} brief(s) across ${tasks.length} reviewer(s)` +
    (skipped.length ? `; ${skipped.length} brief(s) skipped` : '') +
    '.',
)
const sweepAdvisories = []
for (const brief of active) {
  if (brief.shards.length > 1) {
    let line = `Brief "${brief.name}": ${brief.file_set_size} files split across ${brief.shards.length} reviewers.`
    if (brief.sweep_advisory) {
      // A full-extent brief with no declared sweep was split by the per-file
      // default. If it actually checks a cross-file invariant, that split is
      // wrong — surface it so the author can add `sweep: whole-tree`.
      line += ` (full extent, no \`sweep\` declared — if this brief checks a cross-file invariant, add \`sweep: whole-tree\` so it isn't split.)`
      sweepAdvisories.push({ name: brief.name, scope: brief.scope ?? null, shards: brief.shards.length })
    }
    log(line)
  }
}

const results = await parallel(
  tasks.map(({ brief, shard }) => () =>
    agent(buildPrompt(brief, shard), {
      label:
        shard.total > 1
          ? `review:${brief.name} [${shard.index}/${shard.total}]`
          : `review:${brief.name}`,
      phase: 'Review',
      schema: FINDINGS_SCHEMA,
    }),
  ),
)

// Walk the results alongside their tasks (parallel preserves order). A reviewer
// that errored or was skipped resolves to null; with sharding that is a silent
// coverage gap — some of a brief's files went unreviewed, not just a whole brief
// missing — so record each one rather than quietly returning fewer findings.
// Successful reviewers' findings are flattened into one list and tagged with the
// brief and scope; shards of one brief share a name, so they collate under it.
const failures = []
const findings = []
results.forEach((result, i) => {
  const { brief, shard } = tasks[i]
  if (!result) {
    failures.push({
      brief: brief.name,
      shard: `${shard.index}/${shard.total}`,
      scope: brief.scope ?? null,
    })
    log(
      `⚠ reviewer ${shard.index}/${shard.total} for "${brief.name}" returned nothing — ` +
        `its files were not reviewed this run.`,
    )
    return
  }
  for (const finding of result.findings || []) {
    findings.push({ ...finding, brief: result.brief, scope: brief.scope ?? null })
  }
})

return { findings, skipped, failures, sweep_advisories: sweepAdvisories, mode, pr: plan.pr }
