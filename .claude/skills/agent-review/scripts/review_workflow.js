export const meta = {
  name: 'agent-review',
  description: 'Fan out one review agent per .review/ brief and collate their findings',
  phases: [
    { title: 'Review', detail: 'one clean-context agent per brief' },
  ],
}

// args is { plan, promptTemplate } — the plan from plan_review.py and the text
// of prompts/review-brief.md, both read inline by the skill before invoking.
// Workflow scripts cannot read files, so the skill passes everything in; each
// reviewer agent reads its own brief file via its own tools.
const plan = args.plan
const promptTemplate = args.promptTemplate
const root = plan.repo_root
const mode = plan.mode

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
        required: ['file', 'severity', 'message'],
        properties: {
          file: { type: 'string' },
          line: { type: ['integer', 'null'] },
          side: { type: 'string', enum: ['RIGHT', 'LEFT'] },
          severity: { type: 'string', enum: ['high', 'medium', 'low'] },
          message: { type: 'string' },
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

function fill(template, values) {
  let out = template
  for (const [key, value] of Object.entries(values)) {
    out = out.split(`{{${key}}}`).join(value)
  }
  return out
}

function scopeDescription(brief) {
  return brief.scope
    ? `Files under \`${brief.scope}/\`. Only review code within this subtree.`
    : 'The entire repository.'
}

function fileList(brief) {
  return (brief.target_files || []).map((f) => `- ${f}`).join('\n')
}

function targetDescription(brief) {
  const count = (brief.target_files || []).length

  // A --full run audits everything and reports to chat, so no PR classification
  // is needed — just review the manifest.
  if (mode === 'full') {
    return [
      `This is a full conformance sweep, not a diff review. Review every file listed below against your brief; read each one rather than sampling.`,
      `The in-scope files (${count}) are:\n${fileList(brief)}`,
    ].join('\n\n')
  }

  // Diff run, full-extent brief: audit the whole scope, but still classify each
  // finding against the branch so change-caused ones reach the PR and the rest
  // go to the user separately. The brief's own `extent: full` puts it here.
  if (brief.extent === 'full') {
    const base = plan.base_ref
    return [
      `Your brief audits its whole scope, not only what changed — so review every file listed below against it, reading each rather than sampling.`,
      `This branch is being compared against the base \`${base}\`. Classify each violation you find: if it is a product of this branch's changes — it did not exist before the branch, or the branch's edits newly caused it (inspect \`git diff ${base}...HEAD\` to judge) — set \`preexisting: false\`; it belongs on the PR. If it exists independently of this branch, set \`preexisting: true\`; it is reported to the user separately, not on the PR. Report both kinds.`,
      `The in-scope files (${count}) are:\n${fileList(brief)}`,
    ].join('\n\n')
  }

  // Diff run, diff-extent brief (the common case): review only what changed, and
  // judge the violations the branch introduces or touches. Reviewers get the base
  // ref so findings land on lines that exist in the diff.
  const base = plan.base_ref
  return [
    `Review the changes introduced on the current branch against your brief. They are being compared against the base \`${base}\`.`,
    `Inspect the actual diff for each changed file in your scope — for example \`git diff ${base}...HEAD -- <file>\` — and judge the violations the branch introduces or touches. Set \`preexisting: false\` (or omit it) on these.`,
    `Do not go hunting for pre-existing problems. But if, while reading the diff and its surrounding context, you happen to notice an existing violation of your brief on lines the branch did not change, include it as a finding with \`preexisting: true\`. These are surfaced to the user separately and never posted to the PR, so flag them rather than staying silent.`,
    `The changed files within your scope are:\n${fileList(brief)}`,
  ].join('\n\n')
}

// Briefs with a skip_reason (in diff mode: no changed files in scope) don't run.
const active = plan.briefs.filter((b) => !b.skip_reason)
const skipped = plan.briefs
  .filter((b) => b.skip_reason)
  .map((b) => ({ name: b.name, scope: b.scope, reason: b.skip_reason }))

phase('Review')

const results = await parallel(
  active.map((brief) => () => {
    const prompt = fill(promptTemplate, {
      BRIEF_NAME: brief.name,
      BRIEF_PATH: `${root}/${brief.path}`,
      REPO_ROOT: root,
      SCOPE_DESCRIPTION: scopeDescription(brief),
      TARGET_DESCRIPTION: targetDescription(brief),
    })
    return agent(prompt, {
      label: `review:${brief.name}`,
      phase: 'Review',
      schema: FINDINGS_SCHEMA,
    }).then((r) => ({ ...r, scope: brief.scope ?? null }))
  }),
)

// Flatten every reviewer's findings into one list, tagging each with its brief
// and scope so the skill can group and post them. Failed agents resolve to null.
const findings = []
for (const result of results.filter(Boolean)) {
  for (const finding of result.findings || []) {
    findings.push({ ...finding, brief: result.brief, scope: result.scope })
  }
}

return { findings, skipped, mode, pr: plan.pr }
