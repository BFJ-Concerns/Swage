export const meta = {
  name: "shakedown-judgement-wave",
  description:
    "Fan judgement over imported session evidence: per session, a cross-family baseline sweep (readability screen, tiered transcription, charter check) and a taste-tier deep pass over flagged captures plus a pre-declared unflagged sample — concurrent where separately dispatched, collapsed into one combined judge for low-risk charters. Returns structured findings and coverage verdicts for the seat to file; rejections and escalations stay the seat's.",
};

export const defaults = {
  // Judges run cross-family from the driver: the wave input names the
  // driver family, and every judge leg resolves to the other family's
  // strong mid tier unless the staffing overrides name a model. The deep
  // pass is priced by the taste column and its tier is the staffing
  // table's to name per wave — these are the sweep-tier floors.
  effort: "medium",
};

const JUDGE_MODEL_BY_DRIVER_FAMILY = { gpt: "claude-opus-5", claude: "gpt-5.6-terra" };

// INPUT (workflow args):
// {
//   "driverFamily": "gpt"|"claude",     // judges resolve to the OTHER family
//   "findingsRef": "…",                 // this skill's references/findings.md
//                                       // text, inline — judges read
//                                       // severity vocabulary and
//                                       // null-return rules from it
//   "sweep":  { "model"?, "effort"? },  // staffing overrides for the sweep
//   "deep":   { "model"?, "effort"? },  // and the deep pass (taste tier),
//                                       // stated once from the staffing table
//   "sessions": [{
//     "id": "slug/2026-08-31-a",        // names the slot in the result
//     "evidenceDir": "/abs/...",        // the IMPORTED session evidence in
//                                       // the annexe (judges read the
//                                       // imported copy, never the driver's
//                                       // workspace)
//     "charter": "…",                   // the charter the session drove,
//                                       // inline (the sealed script reads
//                                       // no files)
//     "riskTier": "low"|"high",
//     "sampleSize": 4,                  // deep pass: unflagged captures to
//                                       // sample — declared here, before any
//                                       // capture is opened, sized by risk
//                                       // tier; the judge widens to the whole
//                                       // set only when the set is small
//     "timeoutMs"?: 3600000
//   }]
// }
//
// Low-risk sessions run ONE combined judge (both stages' duties, one
// worker); high-risk sessions run the two stages as separate concurrent
// dispatches — both read the same imported tree, neither consumes the
// other's output. Width is the run's (its max_concurrent_agent_calls
// budget), not the script's. Escalation is the seat's: a rejected coverage
// claim or null return re-dispatches between waves, never inside this
// script. Every evidenceDir must sit inside a directory the space has
// attached — placement never widens the run's filesystem authority.
//
// OUTPUT: { sessions: [{ id, sweep, deep, combined }] } — each leg the
// judge's validated JSON or null for an operational failure (the slot is
// named, never dropped; a null leg is a judgement to re-run, never a clean
// pass).

const cfg = args || {};
const driverFamily = cfg.driverFamily;
if (driverFamily !== "gpt" && driverFamily !== "claude")
  throw new Error('judgement-wave needs driverFamily "gpt" or "claude"');
const judgeModel = JUDGE_MODEL_BY_DRIVER_FAMILY[driverFamily];
if (typeof cfg.findingsRef !== "string" || !cfg.findingsRef.trim())
  throw new Error("judgement-wave needs findingsRef — the severity and null-return vocabulary the judges grade against, inline");
const sessions = Array.isArray(cfg.sessions) ? cfg.sessions : [];
if (!sessions.length) throw new Error("judgement-wave needs at least one session");

const seen = new Set();
for (const s of sessions) {
  if (!s || typeof s.id !== "string" || !s.id.trim()) throw new Error("every session needs an id");
  if (seen.has(s.id)) throw new Error(`duplicate session id: ${s.id}`);
  seen.add(s.id);
  if (typeof s.evidenceDir !== "string" || !s.evidenceDir.startsWith("/"))
    throw new Error(`session ${s.id}: evidenceDir must be an absolute path to the imported evidence`);
  if (typeof s.charter !== "string" || !s.charter.trim())
    throw new Error(`session ${s.id}: needs charter (the charter text, inline)`);
  if (s.riskTier !== "low" && s.riskTier !== "high")
    throw new Error(`session ${s.id}: riskTier must be "low" or "high"`);
  if (!Number.isInteger(s.sampleSize) || s.sampleSize < 0)
    throw new Error(`session ${s.id}: sampleSize required — declared before any capture is opened`);
}

const judgeOpts = (s, staffing, leg) => ({
  model: staffing && staffing.model ? staffing.model : judgeModel,
  ...(staffing && staffing.effort ? { effort: staffing.effort } : {}),
  cwd: s.evidenceDir,
  timeoutMs: Number.isInteger(s.timeoutMs) ? s.timeoutMs : 3600000,
  label: `${leg}:${s.id}`,
  phase: leg,
  identity: true,
});

const findingsSchema = {
  type: "object",
  additionalProperties: false,
  required: ["examined", "findings", "coverage_claim", "coverage_claim_why", "out_of_scope_concerns"],
  properties: {
    examined: { type: "string" },
    findings: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["finding", "severity", "evidence"],
        properties: {
          finding: { type: "string" },
          severity: { type: "string" },
          evidence: { type: "string" },
        },
      },
    },
    coverage_claim: { type: "string", enum: ["upheld", "rejected"] },
    coverage_claim_why: { type: "string" },
    out_of_scope_concerns: { type: "string" },
  },
};

const commonBrief = (s) => `Ground rules: read the charter and the severity and null-return vocabulary below first.

<charter>
${s.charter}
</charter>

<findings-vocabulary>
${cfg.findingsRef}
</findings-vocabulary>

The session's imported evidence is this directory (${s.evidenceDir}) — the step record, captures, flags, and oracle-card comparisons; judge the evidence, never the driver's narrative alone, and never modify anything here. Every finding cites its capture or step-record line as evidence. A finding names the observed gap, never a guessed cause. The session's coverage claim is rejected only over a miss you grade High or Critical from the driver's own captures — findings below that bar are filed on their own evidence with the claim upheld. Use out_of_scope_concerns for anything real that fits none of your assigned questions ("" when none).`;

const sweepBrief = (s) => `You are the baseline-sweep judge for a shakedown session, from the family opposite the driver's. ${commonBrief(s)}

The sweep: (1) READABILITY — screen every capture, no exemptions: a capture you cannot read IS a finding, because unreadable output is either a product defect or an evidence defect, and both matter. (2) TRANSCRIPTION — what a user would read from each user-visible capture, at the charter's risk tier (this session: ${s.riskTier}): every capture for a high-risk charter or a surface's first exercise; for low-risk, every flagged capture plus the sample. (3) CHARTER CHECK — depth delivered against the charter, oracle cards used as pre-registered, captures covering the journey; a null return is judged for sufficient depth per the null-return rules.

Reply with the JSON last: examined (what you read and screened, and what you could not), findings (each with severity from the vocabulary and its evidence citation), coverage_claim (upheld/rejected) with coverage_claim_why, out_of_scope_concerns.`;

const deepBrief = (s) => `You are the deep-pass judge for a shakedown session — a demanding early user's eyes, from the family opposite the driver's. ${commonBrief(s)}

The pass runs over the flagged captures plus a sample of ${s.sampleSize} unflagged ones, declared before any capture is opened: list the unflagged captures, pick the sample by position (first/last/evenly spaced), then open them — widen to the whole set only when it is small enough that sampling saves nothing, and say so. Judge: (1) is each surface good, not merely present — layout, hierarchy, noticeability, flow — and where a surface would be materially better arranged another way, say so concretely as an improvement finding; (2) is the product's output actually good against the session's oracle cards — a poor output names the observed gap, never a guessed cause; (3) does the unflagged sample hold anything the driver should have flagged — every miss files as its own finding on its own evidence, and the coverage claim is rejected only per the High/Critical bar.

Reply with the JSON last: examined (the declared sample and what you opened), findings, coverage_claim with coverage_claim_why, out_of_scope_concerns.`;

const combinedBrief = (s) => `You are the single judgement pass for a low-risk shakedown session, from the family opposite the driver's — one worker carrying both stages' duties. ${commonBrief(s)}

First the baseline sweep: (1) READABILITY — screen every capture, no exemptions: a capture you cannot read IS a finding; (2) TRANSCRIPTION — what a user would read, for every flagged capture plus your sample; (3) CHARTER CHECK — depth against the charter, oracle cards as pre-registered, captures covering the journey; a null return is judged for sufficient depth per the null-return rules. Then the deep pass over the flagged captures plus a sample of ${s.sampleSize} unflagged ones, declared before any capture is opened (pick by position, widen only when the set is small and say so): is each surface good, not merely present — concrete improvement findings where a surface would be materially better another way; is the output good against the oracle cards; does the sample hold anything the driver should have flagged.

Reply with the JSON last: examined, findings (each with severity and evidence), coverage_claim with coverage_claim_why, out_of_scope_concerns.`;

phase("judge");

// One thunk list covering every leg: low-risk sessions contribute one
// combined leg; high-risk sessions contribute both stages as separate
// concurrent dispatches over the same imported tree.
const legs = [];
for (const s of sessions) {
  if (s.riskTier === "low") {
    legs.push({ session: s.id, kind: "combined", thunk: () => agent(combinedBrief(s), { ...judgeOpts(s, cfg.sweep, "judge"), schema: findingsSchema }) });
  } else {
    legs.push({ session: s.id, kind: "sweep", thunk: () => agent(sweepBrief(s), { ...judgeOpts(s, cfg.sweep, "sweep"), schema: findingsSchema }) });
    legs.push({ session: s.id, kind: "deep", thunk: () => agent(deepBrief(s), { ...judgeOpts(s, cfg.deep, "deep"), schema: findingsSchema }) });
  }
}
const outputs = await parallel(legs.map((l) => l.thunk));

return {
  sessions: sessions.map((s) => {
    const of = (kind) => {
      const i = legs.findIndex((l) => l.session === s.id && l.kind === kind);
      return i === -1 ? null : (outputs[i] ? outputs[i].output : null);
    };
    return s.riskTier === "low"
      ? { id: s.id, combined: of("combined"), sweep: null, deep: null }
      : { id: s.id, combined: null, sweep: of("sweep"), deep: of("deep") };
  }),
};
