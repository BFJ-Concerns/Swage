export const meta = {
  name: "teardown-audit",
  description:
    "Close a structural phase's teardown accounting: cheap workers blind-sweep the audit window's diff for removed capabilities, then an adjudicator checks them against the phase's restoration obligation records, retired file, and per-teardown sweep records, and writes a durable report. Fails closed when the blind sweep produces nothing usable.",
};

export const defaults = {
  // Cheap volume for the blind sweep; adjudication compares finished lists
  // against records, so it rides a mid tier. Pinned because per-model effort
  // defaults are uneven.
  codex: { model: "gpt-5.6-luna", effort: "low" },
};

// INPUT (--json-args @file):
//   repo:         absolute path to the code repository
//   baseCommit:   the audit window's base — the ledger header's phase base on
//                 a first audit, or the previous audit report's recorded head
//                 hash on a re-entry
//   headCommit:   the audit window's head, resolved to a commit hash BEFORE
//                 the run (git rev-parse HEAD of the handover tree) — the
//                 report records both hashes so the next window's base is
//                 recoverable from the artefacts alone
//   obligationsDir: absolute path to build/obligations/ (the per-obligation
//                 restoration records)
//   retiredPath:  absolute path to build/retired.md
//   teardownsDir: absolute path to build/teardowns/ (per-teardown sweep records)
//   reportPath:   absolute path the adjudicator writes the report to
//   sweeps:       optional blind-sweep worker count (default 3)
//   auditEngine:  "codex" | "claude" — the family OPPOSITE the session
//                 running the structural phase (the audit's independence is
//                 relative to the seat, not absolute); required
//
// Blindness: sweep workers get the repo and the window only — never the
// ledger, retired file, or sweep records; a sweeper that can read the
// records rediscovers them instead of checking them. They are told not to
// consult commit messages or the annexe for the same reason.

const { repo, baseCommit, headCommit, obligationsDir, retiredPath, teardownsDir, reportPath, auditEngine } = args;
if (typeof repo !== "string" || !repo.startsWith("/"))
  throw new Error("teardown-audit: repo is required and must be an absolute path to the code repository");
if (typeof baseCommit !== "string" || !/^[0-9a-f]{7,64}$/i.test(baseCommit))
  throw new Error("teardown-audit: baseCommit is required and must be a 7-64 character hexadecimal commit hash");
if (!headCommit) throw new Error("teardown-audit: headCommit is required — resolve it with git rev-parse before the run");
if (!obligationsDir) throw new Error("teardown-audit: obligationsDir is required — the adjudicator reads the restoration records in build/obligations/");
if (typeof retiredPath !== "string" || !retiredPath.startsWith("/"))
  throw new Error("teardown-audit: retiredPath is required and must be an absolute path to build/retired.md");
if (typeof teardownsDir !== "string" || !teardownsDir.startsWith("/"))
  throw new Error("teardown-audit: teardownsDir is required and must be an absolute path to build/teardowns/");
if (typeof reportPath !== "string" || !reportPath.startsWith("/"))
  throw new Error("teardown-audit: reportPath is required and must be an absolute path for the audit report");
if (auditEngine !== "codex" && auditEngine !== "claude")
  throw new Error("teardown-audit: auditEngine is required ('codex' or 'claude') — pass the family OPPOSITE the seat running the structural phase");
const SWEEP_OPTS = auditEngine === "codex"
  ? { engine: "codex" }                                    // rides defaults: luna/low
  : { engine: "claude", model: "haiku", effort: "low", fallbackModel: "sonnet" };
const ADJ_OPTS = auditEngine === "codex"
  ? { engine: "codex", model: "gpt-5.6-terra", effort: "medium" }
  : { engine: "claude", model: "opus", effort: "medium", fallbackModel: "sonnet" };
const sweeps = Number(args.sweeps) || 3;

phase("blind-sweep");

const capabilitySchema = {
  type: "object",
  additionalProperties: false,
  required: ["capabilities"],
  properties: {
    capabilities: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["capability", "evidence", "confidence"],
        properties: {
          capability: { type: "string" },
          evidence: { type: "string" },
          confidence: { type: "string", enum: ["high", "medium", "low"] },
        },
      },
    },
  },
};

const sweepPrompt = (slice, total) => `You are auditing a repository restructure for silently lost capability. Working directory: ${repo}. Compare the tree at commit ${baseCommit} against ${headCommit}, and list every USER-FACING OR DEPENDED-ON CAPABILITY the old tree provided and the tree at ${headCommit} does not: features, behaviours, commands, endpoints, exported symbols other code consumed, side effects, configuration the old code honoured.

Derive your list from the CODE ONLY: git diff --stat ${baseCommit}..${headCommit} to scope, git show ${baseCommit}:<path> to read removed or gutted files. Do not read commit messages, changelogs, or any files outside this repository — your value is an account independent of the restructuring session's own narrative. Judge capabilities, not lines: a rewrite preserving behaviour is NOT a removal; a deleted file whose behaviour now lives elsewhere is NOT a removal; a surviving file whose behaviour was gutted IS one. Check out or read files at ${headCommit} explicitly rather than trusting the working tree.

You are sweep ${slice} of ${total}: order the changed paths alphabetically and focus on your ${slice}-th portion, reporting anything you notice outside it too.

For each capability: what it was (one line, concrete), evidence (the file/symbol at ${baseCommit} that provided it), confidence it is genuinely gone rather than relocated.

Reply with a single JSON value conforming to the schema. Reason first if you need to, but the JSON value comes last: the final non-whitespace characters of your message must be the JSON value itself — no prose, no code fence, no example after it. Never restate the schema or show a sample answer; the only JSON value in your reply is your actual answer.`;

const sweepsOut = (
  await parallel(
    Array.from({ length: sweeps }, (_, i) => () =>
      agent(sweepPrompt(i + 1, sweeps), {
        ...SWEEP_OPTS,
        schema: capabilitySchema,
        cwd: repo,
        label: `sweep:${i + 1}`,
        phase: "blind-sweep",
      })
    )
  )
).filter(Boolean);

// Fail closed: adjudicating an empty account would produce an apparently
// clean audit from silence. No usable sweep -> no audit. An empty
// capabilities array is only usable when the diff itself is trivial, so
// when every sweep returns empty, verify there was genuinely nothing to
// see rather than trusting silence.
if (sweepsOut.length === 0)
  throw new Error(
    `teardown-audit: all ${sweeps} blind-sweep workers failed or returned nothing usable — the audit cannot run; rerun rather than handing over on silence`
  );
if (sweepsOut.every((s) => s.capabilities.length === 0)) {
  const probe = await agent(
    `Run: git -C ${repo} diff --stat ${baseCommit}..${headCommit} | tail -1 — reply with that summary line only.`,
    { ...SWEEP_OPTS, label: "empty-probe", phase: "blind-sweep" }
  );
  if (!/^\s*$|0 files? changed/.test(probe || ""))
    throw new Error(
      `teardown-audit: every sweep returned an empty capability account but the window's diff is non-trivial (${(probe || "").trim()}) — silent sweeps, not a clean window; rerun`
    );
}

const blindList = sweepsOut.flatMap((s) => s.capabilities);
log(`blind sweep: ${blindList.length} capability claims from ${sweepsOut.length}/${sweeps} workers`);

phase("adjudicate");

const adjudicationSchema = {
  type: "object",
  additionalProperties: false,
  required: ["missing_from_records", "contradicted_records", "unaccounted_sweep_records", "agreed", "summary"],
  properties: {
    missing_from_records: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["capability", "evidence", "note"],
        properties: {
          capability: { type: "string" },
          evidence: { type: "string" },
          note: { type: "string" },
        },
      },
    },
    contradicted_records: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["record", "contradiction"],
        properties: {
          record: { type: "string" },
          contradiction: { type: "string" },
        },
      },
    },
    unaccounted_sweep_records: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["teardown", "gap"],
        properties: {
          teardown: { type: "string" },
          gap: { type: "string" },
        },
      },
    },
    agreed: { type: "array", items: { type: "string" } },
    summary: { type: "string" },
  },
};

const adjudication = await agent(
  `You are adjudicating a teardown audit for the window ${baseCommit}..${headCommit} in ${repo}. The restructuring session kept records of what its removals displaced; independent sweepers have re-derived the removed capabilities blind. Compare all the accounts.

Read yourself:
- ${obligationsDir} — restoration obligations, one record per file
- ${retiredPath} — capabilities explicitly retired, with reasons
- every file under ${teardownsDir} — the per-teardown pre-removal sweep records (area, commit swept, capability list)

Blind sweep claims (deduplicate near-identical claims; weigh each claim's stated confidence):
${JSON.stringify(blindList)}

If ${obligationsDir} holds no records while ${teardownsDir} is non-empty, stop and report that as a blocking discrepancy — an empty restoration account cannot be adjudicated as clean. Where a claim needs checking, check the repository (git show ${baseCommit}:<path>, git log) rather than taking any account's word. The audit covers ACCOUNTING, not just net loss: a capability torn out and already restored within this window is still checked — its teardown record must have dispositioned it. Scope discipline: the obligations directory also carries records that have nothing to do with removals (triage-derived work, the standing assembled-whole record) — judge ONLY records whose Source line points at a teardown record; a non-teardown obligation is outside this audit and is never a contradicted record.

Report:
- missing_from_records: capabilities evidenced as removed that appear in NEITHER the obligation records nor the retired file. The audit's point — candidate silent losses.
- contradicted_records: obligation or retired entries the evidence contradicts (an obligation for something never removed; a retirement whose capability is still present).
- unaccounted_sweep_records: teardown sweep records whose capability lists are not fully covered by obligation records + retired entries — name the teardown and the gap.
- agreed: where the records and the sweep concur (one line each).
- summary: two or three sentences — is the accounting sound, and what must be resolved before handover?

Then write the full report as markdown to ${reportPath}: a header naming the audit window (${baseCommit}..${headCommit}, both literal hashes) and date, then the four sections above with their content, then the summary.

Reply with a single JSON value conforming to the schema. Reason first if you need to, but the JSON value comes last: the final non-whitespace characters of your message must be the JSON value itself — no prose, no code fence, no example after it. Never restate the schema or show a sample answer; the only JSON value in your reply is your actual answer.`,
  {
    ...ADJ_OPTS,
    schema: adjudicationSchema,
    cwd: repo,
    label: "adjudicate",
    phase: "adjudicate",
  }
);

if (!adjudication)
  throw new Error("teardown-audit: adjudication returned nothing usable — rerun; never hand over on silence");

// The report file is the durable artefact the ledger points at; a returned
// JSON with no file behind it would leave the handover citing nothing.
const reportCheck = await agent(
  `If the file ${reportPath} exists and contains the exact string "${baseCommit}..${headCommit}", reply with the single word OK. Otherwise reply MISSING.`,
  { ...SWEEP_OPTS, label: "report-check", phase: "adjudicate" }
);
if (!/\bOK\b/.test(reportCheck || ""))
  throw new Error(`teardown-audit: adjudication returned but ${reportPath} is missing or does not name the window — rerun the adjudication stage`);

return {
  window: `${baseCommit}..${headCommit}`,
  blind_claims: blindList.length,
  sweep_workers: sweepsOut.length,
  report: reportPath,
  adjudication,
};
