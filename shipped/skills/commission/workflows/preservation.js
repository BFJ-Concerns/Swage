// preservation — two-pass cross-family preservation review for a shed or
// materially-rewritten contract, in both directions: pass 1 reviewers (both
// families) BLIND to what changed scan baseline vs candidate for lost intent
// AND extract the candidate's new normative claims; pass 2 adjudicators, now
// given the deliberate-changes record, sort each blind loss finding into
// confirmed loss, accounted-for relocation, justified removal, rejected, or
// insufficient evidence, sort each new claim into settled or unsettled
// addition, and check each claim against the standing contract for
// collisions — and run even on a clean scan, sweeping for losses the blind
// pass missed.

// Worker policy — declared once per family; every agent() call spreads its
// family's pins. Both passes
// compare given texts (scan for loss, adjudicate against the record): decided
// paths that want care rather than search, so the GPT side holds the resting
// level; Claude's blind scan hunts for what a rewrite silently dropped —
// adversarial
// reading, priced at high (references/effort-calibration.md). The GPT side
// pins the flagship: both passes are review work, which runs the flagship
// (references/model-selection.md). Pin model and effort together on every
// call: an unpinned call has no model to ride.
const FAMILY = {
  gpt: { model: "gpt-5.6-sol", effort: "medium" },
  claude: { model: "claude-opus-5", effort: "high" },
};

// Run this before the sign-off read whenever a run shed something or materially rewrote
// existing intent.
//   * Pass 1 is BLIND — reviewers see only baseline and candidate (plus the shed/ files,
//     which are part of the candidate). A reviewer told the story confirms the story and
//     misses the unrelated loss — and a reviewer told which additions were settled
//     extracts only the rest. They must NOT see the cut/keep list, the shed rationale,
//     the settled-items record, or any "this is the final gate" framing. The blind pass
//     reports losses AND extracts every new normative claim the candidate makes.
//   * Pass 2 is SIGHTED — the deliberate-changes record is supplied, so each blind loss
//     finding is adjudicated against what was deliberately moved, dropped, or kept, and
//     each extracted claim against what the conversation actually settled — plus a
//     collision check of each claim against the standing contract.
//
// The main session reinstates confirmed losses into the draft, and takes unsettled
// additions and collisions back through the handshake (settle or remove — never sign
// them through), all before the sign-off read. See references/shedding-workflow.md.
//
// INPUT (pass as the run's args):
//   {
//     "baseline":  "the contract at HEAD (last published) — full text",
//     "candidate": "the working-tree draft — full text",
//     "shed_files": [ { "name": "shed/2026-06-17-foo.md", "content": "..." } ],  // part of the candidate
//     "deliberate_changes": {        // pass 2 ONLY; never shown to pass 1
//       "integrated_delta": "what this session set out to change",
//       "cut_keep_list":    "what beat-2 grounding judged built (shed) vs not-built (kept)",
//       "shed_map":         "each shed item -> the shed/ file it went to",
//       "deliberate_drops": "what was dropped on purpose this run, with the Decision if one was met — including any Decision retired to shed/decisions.md, so a signed retirement adjudicates as justified, not as loss",
//       "settled_items":    "what this session settled through the handshake — each item as the user confirmed it, so an extracted claim with no counterpart here adjudicates as an unsettled addition"
//     }
//   }
//
// OUTPUT (the run's result): { blind_findings[], adjudications: [ { family, adjudications[],
// claim_adjudications[], additional_issues[] } ] } — one entry per adjudicating family.
// The main session reads both entries, reinstates confirmed losses and additional issues,
// and takes unsettled additions and collisions back through the handshake (sign-off
// input, not commentary), surfacing disputed ones to the human.

const cfg = args || {};
const baseline = cfg.baseline || "";
const candidate = cfg.candidate || "";
const shedFiles = Array.isArray(cfg.shed_files) ? cfg.shed_files : [];
const record = cfg.deliberate_changes || {};
if (!baseline.trim() || !candidate.trim()) {
  throw new Error("preservation needs both a baseline and a candidate");
}

const shedBlock = shedFiles.length
  ? shedFiles
      .map((f) => `--- ${f.name} (part of the CANDIDATE) ---\n${f.content}`)
      .join("\n\n")
  : "(no shed/ files this run)";

const blindSchema = {
  type: "object",
  additionalProperties: false,
  required: [
    "coverage",
    "intent_protected",
    "concerns",
    "new_claims",
    "rejected_concerns",
    "insufficient_evidence",
  ],
  properties: {
    // Force a coverage statement so a structurally-empty report is detectable.
    coverage: { type: "string" },
    // The inventory the scan judged against — a clean pass must still show it
    // did the work, not just return an empty concern list.
    intent_protected: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["intent", "kind"],
        properties: {
          intent: { type: "string" },
          kind: {
            type: "string",
            enum: ["constraint", "boundary", "quality", "decision", "other"],
          },
        },
      },
    },
    concerns: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["lost_intent", "kind", "where", "baseline_evidence", "why_it_mattered"],
        properties: {
          lost_intent: { type: "string" },
          kind: {
            type: "string",
            enum: ["constraint", "boundary", "quality", "decision", "other"],
          },
          where: { type: "string" },
          baseline_evidence: { type: "string" },
          why_it_mattered: { type: "string" },
        },
      },
    },
    // The other direction: normative claims present in the candidate and absent
    // from the baseline. Extraction, not judgement — the sighted pass decides
    // which were settled.
    new_claims: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["claim", "kind", "where", "candidate_evidence", "builder_effect"],
        properties: {
          claim: { type: "string" },
          kind: {
            type: "string",
            enum: ["constraint", "boundary", "quality", "decision", "other"],
          },
          where: { type: "string" },
          candidate_evidence: { type: "string" },
          // What a builder would do differently because this sentence exists —
          // the test that separates a normative claim from rewording.
          builder_effect: { type: "string" },
        },
      },
    },
    // Concerns weighed and dropped, with reasons — the reasoning burden a bare
    // verdict would hide.
    rejected_concerns: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["concern", "reason"],
        properties: {
          concern: { type: "string" },
          reason: { type: "string" },
        },
      },
    },
    // Areas that looked risky but could not support a real loss from the text.
    insufficient_evidence: { type: "array", items: { type: "string" } },
  },
};

const adjudicationSchema = {
  type: "object",
  additionalProperties: false,
  required: ["adjudications", "claim_adjudications", "additional_issues", "record_concerns"],
  properties: {
    // One verdict per extracted new claim: was it settled, and does it collide?
    claim_adjudications: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["claim", "verdict", "collision", "confidence", "basis", "recommended_action"],
        properties: {
          claim: { type: "string" },
          verdict: {
            type: "string",
            enum: [
              "settled-addition",
              "unsettled-addition",
              "rejected",
              "insufficient-evidence",
            ],
          },
          // Standing contract text this claim contradicts, quoted — settled
          // claims can collide too, so this is judged for every verdict.
          // Empty string when none.
          collision: { type: "string" },
          confidence: { type: "string", enum: ["high", "medium", "low"] },
          basis: { type: "string" },
          // What to do about it — empty string for verdicts needing no action.
          recommended_action: { type: "string" },
        },
      },
    },
    adjudications: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["concern", "verdict", "confidence", "basis", "recommended_action"],
        properties: {
          concern: { type: "string" },
          verdict: {
            type: "string",
            enum: [
              "confirmed-loss",
              "accounted-relocation",
              "justified-removal",
              "rejected",
              "insufficient-evidence",
            ],
          },
          confidence: { type: "string", enum: ["high", "medium", "low"] },
          basis: { type: "string" },
          // What to do about it — empty string for verdicts needing no action.
          recommended_action: { type: "string" },
        },
      },
    },
    // Material losses the blind pass clearly missed, found with the record in
    // view — a small number, each evidenced, never a fresh generic comparison.
    additional_issues: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["issue", "where", "evidence", "why_it_matters"],
        properties: {
          issue: { type: "string" },
          where: { type: "string" },
          evidence: { type: "string" },
          why_it_matters: { type: "string" },
        },
      },
    },
    // Somewhere to object to the PREMISE rather than the concerns — a
    // deliberate-changes record that is internally contradictory, mismatched
    // to the packages, or wrong in a way the per-concern verdicts cannot
    // express. Required with an explicit empty-string convention: a
    // schema-bound worker is compelled to emit every declared property, so an
    // optional field would invite invented content over an honest blank.
    record_concerns: { type: "string" },
  },
};

const blindPrompt = () =>
  `You are comparing two versions of a software project contract in both directions — for
LOST intent and for NEW normative claims. You are blind to what changed and why — that is
deliberate; judge only the two texts.

BASELINE (the contract as last published):
<baseline>
${baseline}
</baseline>

CANDIDATE (the proposed new version):
<candidate>
${candidate}
</candidate>

These shed/ files are part of the CANDIDATE — detail moved here has relocated, not left, so
treat it as present in the candidate, not missing:
${shedBlock}

First establish what the baseline was protecting: inventory its behaviour-bearing intent as
\`intent_protected\`, each item tagged by kind — constraint, boundary, quality, decision, or
other. Then compare the candidate against that inventory. Judge by intent, not by line: a
detail that was reworded or relocated to a shed/ file is still present; a loss is intent no
longer present in ANY home of the candidate.

Report as \`concerns\` the intent that is MISSING from the candidate, materially WEAKER, or
relocated somewhere materially harder to find. For each: name the lost intent, its kind, where
it lived in the baseline, the baseline text as evidence (quoted), and why it mattered.

Then run the same comparison the other way. Report as \`new_claims\` every normative claim the
candidate makes that the baseline did not: a sentence that would change what a builder does —
a rule, a limit, a terminal outcome, a prohibition, a required behaviour. The test is
\`builder_effect\`: state what a builder implementing the candidate would do differently
because that sentence exists; if nothing would differ, it is rewording, not a claim — leave it
out. Extract every claim that passes the test, quoting the candidate text as evidence; whether
a claim was legitimately settled is not yours to judge and not inferable from the texts, so do
not filter on it. A claim buried mid-paragraph in a large rewrite counts exactly as much as a
prominent one — those are the ones this extraction exists to surface.

Record the concerns you weighed and dropped in \`rejected_concerns\` with reasons, and anything
that looked risky but could not support a real loss in \`insufficient_evidence\`. Discipline:
evidence over vibe — "feels thinner" is useless; do not invent losses or claims; "could not
determine" is a valid finding. State your coverage (which parts of the baseline and candidate
you checked) even if you find nothing — a clean scan that covers both files and shows its
inventory and reasoning is a valid result; bare empty lists are not.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Fan-in guard: the blind-concern union grows with what the reviewers found, and one
// adjudicator fed tens of KB degrades to retries, then null. Concerns adjudicate
// independently, so batch: over the bound, each adjudicator takes size-bounded batches
// (every batch still sees the full record and shed/ files) and the verdicts union. The
// additional-issues sweep needs sight of the WHOLE concern set — a batch cannot know
// what the others covered — so when batching fires it runs as its own call per family
// over the full union projected down to compact fields; in the single-batch case the
// one call does both, as the sweep and the batch see the same complete set.
// Measured in string length — a close proxy for bytes on this mostly-ASCII JSON,
// and the bound is a degradation heuristic, not a protocol limit.
const ADJUDICATE_INPUT_BOUND = 24_000;
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

const adjudicationPrompt = (concerns, sweep) =>
  `You are adjudicating blind loss-review concerns about a contract rewrite, now WITH the
record of what was changed on purpose. For each concern decide which it is.
Return \`claim_adjudications\` as an empty array — a separate call adjudicates the
extracted new claims.

BASELINE (the contract as last published):
<baseline>
${baseline}
</baseline>

CANDIDATE (the proposed new version):
<candidate>
${candidate}
</candidate>

The deliberate-changes record:
${JSON.stringify(record, null, 2)}

The shed/ files (relocated detail, part of the candidate):
${shedBlock}

Blind concerns to adjudicate:
${JSON.stringify(concerns, null, 2)}

For each concern, a verdict:
- "accounted-relocation": compressed in the contract, full detail present in a shed/ file. Not lost.
- "justified-removal": deliberately dropped this run per the record (with its Decision, if
  one was met). A removal that took out live intent — a constraint, boundary, or decision
  nothing in the record actually retired — is a confirmed loss, not a justified one.
- "confirmed-loss": still wanted, gone from BOTH contract and shed/, not deliberately dropped. The bug this exists to catch.
- "rejected": the concern does not survive contact with the texts or the record.
- "insufficient-evidence": cannot tell from what is provided.
Give the basis and your confidence for each verdict, and a recommended action for every
confirmed loss (empty for verdicts needing none). Wording that changed while behaviour,
discoverability and constraints held is not a loss.

${
  sweep
    ? `Only after adjudicating every concern, add \`additional_issues\`: a small number of material
losses the blind pass clearly missed, found while checking the texts against the record —
each with where it lived, quoted evidence, and why it matters — never a fresh generic
comparison. An empty list is the normal result.`
    : `Return \`additional_issues\` as an empty array — a separate whole-set sweep owns that step.`
}

If something is wrong with the deliberate-changes RECORD itself — it contradicts the texts,
contradicts itself, or is wrong in a way the per-concern verdicts cannot express — say so in
\`record_concerns\`; use an empty string when you have nothing, and never pad it.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// The gains direction's sighted half: every extracted claim, judged against what the
// conversation settled and against the standing contract. Runs once per family — the
// claim set is the union of both blind extractions, deduplicated by the adjudicator
// (two families extracting the same sentence is convergence, not two claims).
const claimsPrompt = (claims) =>
  `You are adjudicating NEW normative claims a blind pass extracted from a contract
rewrite — sentences the candidate contains that the baseline did not, each one something
a builder would act on. You now have the record of what this session's conversation
actually settled with the user. For each claim decide which it is.

BASELINE (the contract as last published):
<baseline>
${baseline}
</baseline>

CANDIDATE (the proposed new version):
<candidate>
${candidate}
</candidate>

The deliberate-changes record (\`settled_items\` lists what the conversation settled):
${JSON.stringify(record, null, 2)}

The extracted claims (from both blind reviewers — the same sentence extracted twice is
one claim; adjudicate it once):
${JSON.stringify(claims, null, 2)}

For each claim, a verdict:
- "settled-addition": a settled item in the record covers this claim's substance — not
  merely its topic. A settled item authorises what the user actually confirmed; a claim
  that goes beyond it (a stronger rule, a wider scope, a new terminal outcome) is
  unsettled in the part that goes beyond.
- "unsettled-addition": no settled item accounts for it. The bug this direction exists to
  catch: a rule entering the contract without ever being put to the user.
- "rejected": not actually normative, or present in the baseline after all.
- "insufficient-evidence": cannot tell from what is provided.

For EVERY claim, whatever its verdict, also check \`collision\`: quote any standing
contract text — candidate or baseline — that this claim contradicts or pulls against
(one rule says repair, another says terminate; a limit here, an exemption there). A
settled claim can still collide. Empty string when none.

Give the basis and your confidence for each verdict, and a recommended action for every
unsettled addition or collision (empty for verdicts needing none). Return
\`adjudications\` and \`additional_issues\` as empty arrays, and \`record_concerns\` as
usual — an empty string unless the record itself is contradictory or mismatched.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// The batched path's sweep: sees every blind concern (projected to compact fields so
// the input stays bounded) plus the record and shed/ files, and hunts only for what
// the whole set missed. Adjudications belong to the batches; this call returns none.
const sweepPrompt = (allConcerns) =>
  `You are reviewing a contract rewrite WITH the record of what was changed on purpose. A
separate adjudication has already judged each blind concern below — do not re-adjudicate.

BASELINE (the contract as last published):
<baseline>
${baseline}
</baseline>

CANDIDATE (the proposed new version):
<candidate>
${candidate}
</candidate>

The deliberate-changes record:
${JSON.stringify(record, null, 2)}

The shed/ files (relocated detail, part of the candidate):
${shedBlock}

Every concern the blind pass raised (compact view — coverage of the concern space, so you
can see what it did NOT think of):
${JSON.stringify(allConcerns, null, 2)}

Return \`adjudications\` and \`claim_adjudications\` as empty arrays. In
\`additional_issues\`, report a small number of material losses the blind pass clearly
missed, found while checking the texts against the record — each with where it lived,
quoted evidence, and why it matters — never a fresh generic comparison. An empty list is
the normal result.

If something is wrong with the deliberate-changes RECORD itself — it contradicts the texts,
contradicts itself, or is wrong in a way the verdicts cannot express — say so in
\`record_concerns\`; use an empty string when you have nothing, and never pad it.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Pass 1: both families scan blind, in parallel. Barrier before adjudication — pass 2
// waits for the full concern union, then feeds it to each family in bounded batches.
phase("blind-scan");
const blind = (
  await parallel(
    ["gpt", "claude"].map((family) => () =>
      agent(blindPrompt(), {
        ...FAMILY[family],
        schema: blindSchema,
        label: `blind:${family}`,
      }).then((b) => (b ? { family, ...b } : null)),
    ),
  )
).filter(Boolean);

const concerns = blind.flatMap((b) =>
  (b.concerns || []).map((c) => ({ ...c, by: b.family })),
);
const newClaims = blind.flatMap((b) =>
  (b.new_claims || []).map((c) => ({ ...c, by: b.family })),
);
log(
  `${concerns.length} blind concerns, ${newClaims.length} extracted claims from ${blind.length}/2 reviewers`,
);

// Pass 2 runs even on a clean scan: with the record in view it still sweeps for
// material losses the blind pass missed — a clean blind scan is a valid result,
// not a reason to skip the sighted check.
const concernBatches = concerns.length
  ? batchBySize(concerns, ADJUDICATE_INPUT_BOUND)
  : [
      "(the blind pass raised no concerns — adjudicate it as a clean pass and run the additional-issues sweep)",
    ];
if (concernBatches.length > 1) {
  log(`adjudicating in ${concernBatches.length} size-bounded batches per family`);
}

// Pass 2: both families adjudicate the full concern set against the record — in
// bounded batches, verdicts unioned per family. Single batch: that one call also
// runs the sweep (it sees the complete set). Multiple batches: no batch sweeps;
// a dedicated whole-set sweep call runs alongside them.
phase("adjudicate");
const singleBatch = concernBatches.length === 1;
const sweepView = concerns.map(({ lost_intent, kind, where, by }) => ({
  lost_intent,
  kind,
  where,
  by,
}));
const adjudicate = async (family) => {
  const runs = concernBatches.map((batch, k) => () =>
    agent(adjudicationPrompt(batch, singleBatch), {
      ...FAMILY[family],
      schema: adjudicationSchema,
      label: `adjudicate:${family}:${k}`,
    }),
  );
  if (!singleBatch) {
    runs.push(() =>
      agent(sweepPrompt(sweepView), {
        ...FAMILY[family],
        schema: adjudicationSchema,
        label: `sweep:${family}`,
      }),
    );
  }
  // The gains direction: one dedicated call per family over the full claim
  // union — settled-vs-unsettled needs the record, collisions need the whole
  // contract, and the claim set is small enough not to batch.
  if (newClaims.length) {
    runs.push(() =>
      agent(claimsPrompt(newClaims), {
        ...FAMILY[family],
        schema: adjudicationSchema,
        label: `claims:${family}`,
      }),
    );
  }
  const parts = (await parallel(runs)).filter(Boolean);
  if (parts.length === 0) return null;
  return {
    family,
    adjudications: parts.flatMap((p) => p.adjudications || []),
    claim_adjudications: parts.flatMap((p) => p.claim_adjudications || []),
    additional_issues: parts.flatMap((p) => p.additional_issues || []),
    record_concerns: parts
      .map((p) => (p.record_concerns || "").trim())
      .filter(Boolean)
      .join("\n"),
  };
};

const adjudications = (
  await parallel(["gpt", "claude"].map((family) => () => adjudicate(family)))
).filter(Boolean);

// An adjudicator's objection to the record is exactly what a clean-looking
// verdict set would otherwise bury — surface it before the verdicts are trusted.
for (const a of adjudications) {
  if (a.record_concerns) {
    log(`adjudicate:${a.family} raised a concern about the deliberate-changes record itself — read it before acting on the verdicts`);
  }
}

return { blind_findings: blind, adjudications };
