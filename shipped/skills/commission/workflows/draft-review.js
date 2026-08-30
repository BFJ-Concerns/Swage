// draft-review — adversarial review of a near-final contract draft, run
// across both model families and SIGHTED on the settled decisions so it
// attacks faithful encoding, internal consistency and genuine gaps rather
// than relitigating decided trade-offs. Returns findings grouped for the main
// session's reconciliation beat: material findings, reopen-prompts for
// challenged decisions, traceability gaps, and a held-findings count.

// Worker policy — declared once per family; every agent() call spreads its
// family's pins. The lens,
// the draft, and the settled decisions decide most of the attack's path, and
// the merge consolidates a given finding set — so every stage holds the
// resting level, with Claude's adversarial attack legs priced at high
// (references/effort-calibration.md). The GPT side pins the flagship:
// every GPT leg here attacks or adjudicates — review work, which runs the
// flagship (references/model-selection.md). Pin model and effort together on
// every call: an unpinned call has no model to ride.
const FAMILY = {
  gpt: { model: "gpt-5.6-sol", effort: "medium" },
  claude: { model: "claude-opus-5", effort: "high" },
};

// Run this shortly before the sign-off read, scaled by the size of the change.
//   * Attackers SEE: the full draft, the settled decisions (with their examined/provisional
//     status and rationale), and the change delta.
//   * Attackers do NOT decide anything and do NOT relitigate an EXAMINED decision
//     (one the human and main session actually compared against alternatives — a
//     divergent survey ran, or options were weighed in the conversation). They
//     attack whether the draft faithfully encodes the decisions, whether it is internally
//     consistent, and whether there is a real gap the decisions never addressed. A
//     PROVISIONAL decision (named once, never compared) MAY be challenged — it was never a
//     real choice, so it is not shielded.
//
// The main session metabolises the output with the reconciliation beat (see
// `SKILL.md`): a finding that challenges a settled examined decision
// is surfaced to the human as a reopen question, never applied as a silent reversal.
//
// INPUT (pass as the run's args):
//   {
//     "draft": "the full draft contract (README) text",
//     "settled_decisions": [
//       { "id": "d1", "decision": "...", "status": "examined" | "provisional", "rationale": "..." }
//     ],
//     "delta":  "what THIS session changed (scales how hard to push; '' for greenfield)",
//     "lenses": ["...", ...]   // optional attack lenses; sensible defaults below
//   }
//
// OUTPUT (the run's result): { lenses, findings[], reviews[] }. Each raw finding carries the
// lens and model family (`by`) that produced it, so per-lens performance is readable after the
// run. The main session reads the merged reviews, applies the reconciliation beat, and
// relays material items to the human.

const cfg = args || {};
const draft = cfg.draft || "";
const settled = Array.isArray(cfg.settled_decisions) ? cfg.settled_decisions : [];
const delta = cfg.delta || "";
if (!draft.trim()) throw new Error("draft-review needs a non-empty draft");

const DEFAULT_LENSES = [
  "contradiction & consistency — claims that fight each other, or a clause that contradicts a settled decision's recorded rationale",
  "underspecification — places a downstream builder would have to guess, where the contract should pin the outcome",
  "feasibility & hidden commitment — what this quietly forces to be true, depended-on substrate that is unstated, or success criteria that cannot be verified",
  "traceability — every settled decision should be represented in the draft or explicitly out; flag any that vanished or was silently weakened",
  // The holistic lens: the other four attack clause by clause; this one judges the
  // assembled whole, which no earlier stage reads (the divergent survey widens the
  // table per decision, before the draft exists to be read).
  "shape & proportionality — the draft judged as a whole, with every settled decision taken as fixed: complexity, coupling, or scope that no settled decision or stated constraint actually forces, and any simpler overall shape that would still honour all of them",
];
const lenses =
  Array.isArray(cfg.lenses) && cfg.lenses.length ? cfg.lenses : DEFAULT_LENSES;

const decisionsBlock = settled.length
  ? JSON.stringify(settled, null, 2)
  : "(none recorded yet — treat every load-bearing choice in the draft as still open)";

const findingSchema = {
  type: "object",
  additionalProperties: false,
  required: ["findings"],
  properties: {
    findings: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: [
          "finding",
          "where",
          "category",
          "ledger_relation",
          "severity",
          "evidence",
          "suggested_action",
        ],
        properties: {
          finding: { type: "string" },
          where: { type: "string" },
          category: { type: "string" },
          // How this finding relates to what is already settled — drives the main
          // session's reconciliation beat.
          ledger_relation: {
            type: "string",
            enum: [
              "new",
              "challenges-examined",
              "challenges-provisional",
              "supports",
              "redundant",
            ],
          },
          severity: { type: "string", enum: ["blocker", "material", "minor"] },
          evidence: { type: "string" },
          suggested_action: { type: "string" },
        },
      },
    },
  },
};

// The merge groups findings for the reconciliation beat and enforces log-vs-relay:
// minor/redundant are counted, not dumped on the human.
const mergeSchema = {
  type: "object",
  additionalProperties: false,
  required: [
    "material_findings",
    "reopen_prompts",
    "traceability_gaps",
    "held_findings_count",
  ],
  properties: {
    material_findings: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["finding", "where", "category", "severity", "evidence", "suggested_action"],
        properties: {
          finding: { type: "string" },
          where: { type: "string" },
          category: { type: "string" },
          severity: { type: "string", enum: ["blocker", "material"] },
          evidence: { type: "string" },
          suggested_action: { type: "string" },
        },
      },
    },
    // Challenges to settled decisions, framed as QUESTIONS for the human — never
    // applied as reversals. This is the reconciliation contract, made explicit.
    reopen_prompts: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["decision_id", "decision_status", "challenge", "new_information"],
        properties: {
          decision_id: { type: "string" },
          decision_status: { type: "string" },
          challenge: { type: "string" },
          // A settled examined decision reopens only on genuinely new information;
          // name it (or say there is none, which means: do not reopen).
          new_information: { type: "string" },
        },
      },
    },
    traceability_gaps: { type: "array", items: { type: "string" } },
    held_findings_count: { type: "integer" },
  },
};

const attackPrompt = (lens) =>
  `You are an adversarial reviewer of a near-final software project contract. Be sceptical,
not agreeable. Attack through this lens: ${lens}

Here is the draft contract:
<draft>
${draft}
</draft>

Here are the decisions the human and author have ALREADY SETTLED this conversation:
${decisionsBlock}
${delta ? `\nWhat changed THIS session (push hardest here):\n${delta}` : ""}

Rules that make your attack useful rather than noise:
- An EXAMINED decision is decided. Do NOT relitigate whether it was the right call. Attack
  whether the draft faithfully ENCODES it, whether anything CONTRADICTS it, and whether it
  left a real GAP. Set ledger_relation="challenges-examined" ONLY if you have concrete new
  information the decision did not account for — otherwise it is not a finding.
- A PROVISIONAL decision was named but never compared against alternatives. You MAY challenge
  it (ledger_relation="challenges-provisional").
- A genuinely new problem the decisions never touched is ledger_relation="new".
- Attach evidence to every finding — quote the draft text. No evidence, no finding.
- "I could not find a problem here" is a valid, welcome result. There is no quota.

Return findings, each tagged with category, ledger_relation, severity, evidence (quoted),
and a concrete suggested_action.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Fan-in guard: a reduce worker's prompt is the SUM of the upstream outputs, and one
// worker fed tens of KB degrades to retries, then null. Consolidation composes, so
// tree-reduce — merge size-bounded batches, then merge the batch summaries. At the
// usual finding volume there is one batch and this is a single ordinary merge.
// Measured in string length — a close proxy for bytes on this mostly-ASCII JSON,
// and the bound is a degradation heuristic, not a protocol limit.
const MERGE_INPUT_BOUND = 24_000;
const batchBySize = (items, bytes) => {
  const batches = [[]];
  let size = 0;
  for (const item of items) {
    const len = JSON.stringify(item).length;
    if (size + len > bytes && batches[batches.length - 1].length) {
      batches.push([]);
      size = 0;
    }
    batches[batches.length - 1].push(item);
    size += len;
  }
  return batches;
};

const mergePrompt = (findings) =>
  `You are consolidating adversarial findings on a near-final contract for the main planning
session to act on. You did not write the draft. Several reviewers across two model families
produced these findings:
${JSON.stringify(findings, null, 2)}

Produce a clean, de-duplicated set the main session can metabolise:
- material_findings: blocker/material findings only, merged so duplicates collapse to one.
- reopen_prompts: every finding that challenges a settled decision, rewritten as a QUESTION
  for the human ("decision d2 settled X because Y — does <new_information> change that?").
  If a challenge to an EXAMINED decision carries no genuine new information, drop it (do not
  manufacture a reopen). Provisional decisions can be questioned freely.
- traceability_gaps: settled decisions not represented in the draft, or silently weakened.
- held_findings_count: how many minor/redundant/supports-only findings you are NOT surfacing
  (they are logged, not dumped on the human).

Do not invent severity to promote a weak finding. Confident noise is still noise.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Every lens attacked by BOTH families, so no lens is seen by only one kind. No barrier
// before the per-lens attacks; the merge collects them all, then feeds its workers in
// size-bounded batches (see the fan-in guard above) rather than as one raw union.
phase("attack");
const lensRuns = [];
for (let i = 0; i < lenses.length; i++) {
  for (const family of ["gpt", "claude"]) {
    lensRuns.push({ lens: lenses[i], family, i });
  }
}
log(`${lensRuns.length} attacks: ${lenses.length} lens(es) x 2 families`);

const raw = (
  await parallel(
    lensRuns.map((r) => () =>
      agent(attackPrompt(r.lens), {
        ...FAMILY[r.family],
        schema: findingSchema,
        label: `attack:${r.family}:${r.i}`,
      }).then((res) => (res ? { family: r.family, lens: r.lens, ...res } : null)),
    ),
  )
).filter(Boolean);

// Keep lens attribution alongside family attribution: the reconciliation beat
// reads per-lens performance (material vs noise), which is impossible to
// reconstruct once findings are flattened.
const allFindings = raw.flatMap((r) =>
  (r.findings || []).map((f) => ({ ...f, lens: r.lens, by: r.family })),
);
log(`${allFindings.length} raw findings from ${raw.length}/${lensRuns.length} attackers`);

const remergePrompt = (partials) =>
  `You are combining several PARTIAL consolidations of adversarial findings on a contract
draft into one. Each partial already de-duplicated its own batch; your job is the cross-batch
pass: collapse duplicates that landed in different partials, keep the union of everything
else, and sum the held_findings_count values. Do not re-judge severity or invent findings —
consolidate what is here.

The partial consolidations:
${JSON.stringify(partials, null, 2)}

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Cross-family merge: both families consolidate, so neither curates only its own findings.
// Each family's merge is a tree-reduce: size-bounded batches of raw findings first, then —
// only when there was more than one batch — a re-merge of the partials, which are far
// smaller than the raw union (deduped, material-only) and safely fit one worker.
phase("merge");
const treeMerge = async (family) => {
  const batches = batchBySize(allFindings, MERGE_INPUT_BOUND);
  if (batches.length > 1) log(`merge:${family} tree-reducing in ${batches.length} batches`);
  let level = (
    await parallel(
      batches.map((batch, k) => () =>
        agent(mergePrompt(batch), {
          ...FAMILY[family],
          schema: mergeSchema,
          label: `merge:${family}:${k}`,
        }),
      ),
    )
  ).filter(Boolean);
  // Reduce the partials in bounded rounds too — one giant re-merge would just
  // re-create the ceiling a level up. Deduped partials shrink fast, so this
  // almost always resolves in a single round.
  let round = 0;
  while (level.length > 1) {
    round += 1;
    const groups = batchBySize(level, MERGE_INPUT_BOUND);
    level = (
      await parallel(
        groups.map((group, k) => () =>
          group.length === 1
            ? Promise.resolve(group[0])
            : agent(remergePrompt(group), {
                ...FAMILY[family],
                schema: mergeSchema,
                label: `remerge:${family}:${round}:${k}`,
              }),
        ),
      )
    ).filter(Boolean);
    if (level.length === groups.length && groups.length > 1 && groups.every((g) => g.length === 1)) {
      // No group could shrink further (every partial alone exceeds the bound);
      // stop rather than loop — the last level still unions below.
      log(`merge:${family} tree-reduce stalled at ${level.length} irreducible partials — falling back to mechanical union`);
      break;
    }
  }
  if (level.length === 0) return null;
  if (level.length === 1) return level[0];
  // Irreducible remainder: union mechanically rather than lose findings.
  return {
    material_findings: level.flatMap((p) => p.material_findings || []),
    reopen_prompts: level.flatMap((p) => p.reopen_prompts || []),
    traceability_gaps: level.flatMap((p) => p.traceability_gaps || []),
    held_findings_count: level.reduce((n, p) => n + (p.held_findings_count || 0), 0),
  };
};

const reviews = (
  await parallel(
    ["gpt", "claude"].map((family) => () =>
      treeMerge(family).then((m) => (m ? { family, ...m } : null)),
    ),
  )
).filter(Boolean);

return { lenses, findings: allFindings, reviews };
