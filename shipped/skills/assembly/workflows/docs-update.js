// docs-update — the assembly skill's documentation pipeline: restatement
// (hold), a docs refresh written through the project-docs skill,
// cross-family accuracy verification against the tree, autonomous fix
// rounds that converge severe findings before the per-finding triage hold,
// then a landing stage that merges the unit directly — no PR under either
// merge policy. The build-verify shape with the writer and verifier
// families swapped.

export const defaults = {
  // Families swap relative to build-verify: the writer is a Claude leg
  // (the default below) and verification crosses to a GPT model pinned per
  // call, below its family's top tier as verifiers always are. The writer
  // pin is operator-settled: change it only on the user's word, never as a
  // modernisation sweep. Writer effort is medium: the brief names the
  // covered units, but what the drift actually is and where each fact's
  // canonical home lies are the writer's own calls. The writer is one held
  // worker — restatement, the write, and every fix round are turns of a
  // single session — and so is the verifier, so each re-verification
  // returns to the reader that derived the claim list; the landing is a
  // different model's job and re-briefs.
  model: "claude-opus-5-5",
  effort: "medium",
};

// INPUT (workflow args):
//   briefPath, worktree, unitDir, mergePolicy ("direct-merge"|"pr"),
//   integrationBranch (PR mode: required — the branch the unit merges into:
//   the structural branch, or main for a `Lane: main` unit), annexeConfigPath
//   (config.toml, for push_policy)
//
// The brief names the docs obligation served and the landed units covered
// (SKILL.md, Documentation Rides the Run); docs units carry no spine
// exemplar, so the restatement holds no admission check.
//
// HOLD PROTOCOL: identical to build-verify.js — the hold posts a
// "decision.needed" event carrying its exact content and pauses on the
// run's event queue until the answering "decision.answer" event (lead segment or
// operator, via the external post surface) echoes the hold's nonce. No
// hold timeout: an unanswered decision blocks, and the journal carries
// every completed stage.

const { briefPath, worktree, unitDir, mergePolicy, annexeConfigPath, integrationBranch } = args;
if (mergePolicy !== "direct-merge" && mergePolicy !== "pr")
  throw new Error(`unknown mergePolicy: ${JSON.stringify(mergePolicy)} (expected "direct-merge" or "pr")`);
if (mergePolicy === "pr" && !integrationBranch)
  throw new Error('mergePolicy "pr" requires integrationBranch (the structural branch unit PRs target)');

// ------------------------------------------------------- dispatch record
// Every dispatch records on the run journal whether it continued a held
// worker or re-briefed a fresh one, and why — the choice is this script's,
// per stage: a later turn that profits from accumulated context continues
// the held worker; a stage whose value is independence, or whose envelope
// differs from the worker's, re-briefs. `staleness()` is journalled, so a
// decision taken on it replays identically.
const dispatching = (label, mode, reason) => log(`[dispatch] ${label}: ${mode} — ${reason}`);
const idleReading = async (worker) => {
  const reading = await staleness(worker);
  const phrase = reading.stale === true
    ? `idle ${reading.idleMs}ms, past the ${reading.thresholdMs}ms threshold of route ${reading.route}`
    : reading.stale === false
      ? `idle ${reading.idleMs}ms, within the ${reading.thresholdMs}ms threshold of route ${reading.route}`
      : `idle ${reading.idleMs}ms, no staleness threshold declared on route ${reading.route ?? "unknown"}`;
  return { ...reading, phrase };
};

// One nonce per hold from a deterministic counter (the sealed context has
// no clock or randomness): a stale decision from an earlier attempt names
// a different nonce and is ignored. The decision event's payload contract,
// validated by the script itself:
//   { nonce, verdict: one of the offered options,
//     note?: string,
//     dispositions?: [{ finding: int, disposition: "fix" |
//       "contract-question" | "accept-with-note", note?: string }] }
let holdCount = 0;
const mintNonce = (stage) => `${stage}-${++holdCount}`;

const runHold = async (stage, content, verdicts) => {
  const nonce = mintNonce(stage);
  // Raise the decision on the run's event queue and park until answered —
  // by a lead segment clearing its batch of decisions at a wake, or by the
  // operator through the run surface. Blocking is accepted: no timeout.
  const request = await postEvent("decision.needed", { stage, nonce, content, options: verdicts });
  while (true) {
    const decision = await awaitEvent({ kinds: ["decision.answer"] });
    const payload = decision && decision.payload;
    if (!payload || payload.nonce !== nonce) {
      log(`[hold] ${stage}: stale or unaddressed decision ignored; still waiting for ${nonce}`);
      continue;
    }
    if (!verdicts.includes(payload.verdict)) {
      log(`[hold] ${stage}: unknown verdict ${JSON.stringify(payload.verdict)} ignored (offered: ${verdicts.join(", ")}); still waiting for ${nonce}`);
      continue;
    }
    if (payload.note !== undefined && typeof payload.note !== "string") {
      log(`[hold] ${stage}: decision note must be a string; still waiting for ${nonce}`);
      continue;
    }
    // Only this answered request is settled; concurrent requests and worker
    // events retain their own acknowledgement lifecycle. Replay retains its seq.
    await deliverEvents({ event_seq: request.seq });
    return payload;
  }
};

// ---------------------------------------------------------------- restate
phase("restate");

const restatementSchema = {
  type: "object",
  additionalProperties: false,
  required: ["restatement", "approach", "disputes"],
  properties: {
    restatement: { type: "string" },
    approach: { type: "string" },
    disputes: { type: "string" },
  },
};

const restatePrompt = (correction) => `Read the brief at ${briefPath}. It names a documentation obligation and the landed units whose changes the docs work covers. Read those units' changes in this worktree (${worktree}) and the repository's existing README and docs. Do not start work.${correction ? `\n\nYour earlier restatement was corrected by the orchestrator: "${correction}". Apply the correction; restate the CHANGED part fully.` : ""}

Reply with: restatement (the brief in your own words — what documentation must end up true, not a paraphrase of its sentences), approach (which documents you expect to touch and why, what looks drifted, what may be newly earned), disputes (anything the brief has wrong — a covered unit that changed nothing documented, docs the brief assumes that do not exist; "" if none). Reply with the JSON last.`;

// The writer is one held worker from restatement onward: the survey it
// makes of the covered units and the existing docs to restate the brief is
// the context the write turn starts from.
const writer = await session({ cwd: worktree, label: "writer" });
let sketch = null;
let holdDecision = null;
const rulings = [];
for (let attempt = 0; attempt < 3; attempt++) {
  dispatching(
    `restate:${attempt}`,
    attempt === 0 ? "re-briefed" : "continued",
    attempt === 0
      ? "the writer's held session opens on the brief; the write and its fix rounds continue this worker"
      : "the orchestrator corrected this worker's own restatement — it refines what it proposed rather than re-surveying the docs"
  );
  sketch = await send(writer, restatePrompt(holdDecision && holdDecision.note), {
    schema: restatementSchema,
    label: `restate:${attempt}`,
  });
  if (!sketch) throw new Error("restatement stage returned nothing usable");
  holdDecision = await runHold("restate", sketch, ["accept", "correct", "kill"]);
  if (holdDecision.verdict === "kill") return { outcome: "killed-at-restatement", sketch };
  if (holdDecision.note && holdDecision.note.trim()) rulings.push(holdDecision.note.trim());
  if (holdDecision.verdict === "accept") break;
}
if (holdDecision.verdict !== "accept")
  return { outcome: "corrections-exhausted", at: "restate", salvage: { sketch } };

// ---------------------------------------------------------------- write
phase("write");

dispatching("write", "continued", "the write starts from the survey this worker made to restate: the covered units' changes and the existing docs are already in its context");
const writeReport = await send(
  writer,
  `Refresh this repository's documentation as this accepted restatement describes — your own restatement, as the orchestrator accepted it over the brief at ${briefPath}; persist trust in it:

<accepted-restatement>
${JSON.stringify(sketch)}
</accepted-restatement>

Invoke the project-docs skill and run it scoped to the landed changes the brief's covered units name — its own mode selection applies: audit the documentation against the current tree and patch the claims those changes made wrong (refresh), and write documentation that does not yet exist where it is newly earned (a repository without a README gets one), scaled by the skill's earn-its-file gate. First read docs_policy from ${annexeConfigPath} (absent means both) and maintain only the register(s) it names. Where the project keeps the skill's parallel sets, follow its precedence: bring the agent-docs pages true against the code first, re-derive the affected human pages from them, then regenerate the indexes with the skill's doc-indexes script. The skill's evidence, register, and canonical-home doctrine all apply. Do not touch the changelog: changelog entries land with the units that earn them, and editing one here is out of scope.

Work only in this worktree (${worktree}); commit as you go with plain descriptive messages; never push, never open PRs, never merge — publication is the landing stage's, not yours. Modify documentation files only — never product code, and never the agent-guidance file (AGENTS.md / CLAUDE.md): guidance-file changes are the user's to make, so compile any agent-facing guidance facts the landed changes produce into your report as a pending hand-off instead.

Write ${unitDir}/report.md: the drift found (claim by claim, with what made each wrong); what was patched, written, or deliberately left; how you verified the changed claims and what you did NOT check; your overall confidence (high/medium/low) with a one-line why. Reply with a two-sentence summary and the head commit hash.`,
  { label: "write", timeoutMs: 7200000 }
);

// ---------------------------------------------------------------- verify
phase("verify");

const findingsSchema = {
  type: "object",
  additionalProperties: false,
  required: ["examined", "findings", "rejected_concerns", "confidence", "confidence_why"],
  properties: {
    examined: { type: "string" },
    findings: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["finding", "severity", "confidence", "evidence"],
        properties: {
          finding: { type: "string" },
          severity: { type: "string", enum: ["severe", "moderate", "minor"] },
          confidence: { type: "string", enum: ["high", "medium", "low"] },
          evidence: { type: "string" },
          unresolved_prior: { type: "boolean" },
        },
      },
    },
    rejected_concerns: { type: "string" },
    confidence: { type: "string", enum: ["high", "medium", "low"] },
    confidence_why: { type: "string" },
  },
};

const verifyPrompt = (refixRound, fixMode) => `You are the cross-family verifier for a documentation unit. Judge against the brief at ${briefPath}, the accepted restatement, and the orchestrator's rulings below — the latter two hold the brief's standing, and a conflicting ruling supersedes it. Rulings are listed in the order issued; where two conflict, the later governs.
${rulings.length ? `\n<orchestrator-rulings>\n${rulings.map((r, i) => `${i + 1}. ${r}`).join("\n")}\n</orchestrator-rulings>\n` : ""}
<accepted-restatement>
${JSON.stringify(sketch)}
</accepted-restatement>

This is an accuracy audit: documentation claims are checked against their authority, never against the writer's account. Derive the claim list BEFORE reading the writer's report: diff the documentation changes in ${worktree} against their base, and collect every new or changed substantive claim — behaviour, commands, configuration, APIs, architecture, file paths — tagging each with its chain leg. Claims on agent-docs pages, and claims on human pages with no agent counterpart, are checked against the code at HEAD: run what is safely runnable (--help, read-only commands), read the source for the rest. Claims on human pages that mirror an agent-docs counterpart are checked against that counterpart — same facts, no contradictions, nothing invented in the retelling — and when the counterpart was NOT itself updated in this unit, additionally confirm the counterpart's claim against the code before accepting the derivation: a faithful copy of a stale agent page is not verified. Where the counterpart contradicts the code, that is the finding, not the copy. Where the project keeps parallel sets, run the project-docs skill's doc-indexes script with --check: a stale index or an undeclared one-sided page is a finding. Also judge coverage the other way: did the covered units the brief names change documented reality in ways the docs still miss? Only after that, read ${unitDir}/report.md and audit its account against what you found.${refixRound !== null ? `\n\nThis is a RE-VERIFICATION after a fix round. ${fixMode === "autonomous" ? "The round ran autonomously — no orchestrator dispositions exist yet: every finding of the earlier verdict was dispatched for fix, and the fixer could leave one unfixed only by declaring it blocked in the report." : 'The orchestrator dispositioned the earlier findings; only those marked "fix" were dispatched.'} Focus on: (1) each finding dispatched for fix — is it genuinely resolved, with the fix commits as evidence — setting unresolved_prior true on any finding you now report that restates one the fix round failed to resolve; (2) whether the fix delta introduced new wrong claims, re-examining what your earlier verdict said it examined; (3) any fix the report declares deliberately left unapplied as blocked — judge its grounds and surface it as its own finding for the orchestrator's disposition, not as a failed fix. Earlier verdict: ${unitDir}/${refixRound === 0 ? "verdict.md" : `verdict-refix-${refixRound - 1}.md`}.` : ""}

A claim the code contradicts is a finding, whatever the prose quality; a documented command you could not verify is a finding at the confidence your check earned; docs written beyond what the covered changes and the brief warrant are a finding (overbuild). Changelog entries are out of this unit's scope — their absence here is not a finding. Product code or the agent-guidance file (AGENTS.md / CLAUDE.md) modified by this unit is a severe finding: the unit's writ runs over documentation files only. Hunt for why the docs are wrong, not for reasons to agree.

Every command your verdict leans on is evidenced by the command line as executed plus enough of its actual output to support the claim — or an explicit "could not run: <reason>"; a run cited with neither is an unevidenced claim, not a completed check. Report to the findings contract: examined (what you read, ran, and checked — and what you could not), findings (each with severity, confidence, a verbatim evidence quote or command output, and unresolved_prior — true only when it restates a finding from the earlier verdict that its fix round failed to resolve, false otherwise, and always false on a first verification), rejected_concerns, confidence in your own read (bounded by the unseen list) and its one-line why. Write the same content to ${unitDir}/${refixRound !== null ? `verdict-refix-${refixRound}.md` : "verdict.md"}.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show a sample answer; the only JSON value in your reply is your actual answer.`;

// The verifier is a held worker of the opposite family: opened blind to the
// writer's session, then continued for every re-verification.
const verifier = await session({ model: "gpt-6.1-sol", cwd: worktree, label: "verifier" });
dispatching("verify", "re-briefed", "cross-family accuracy audit is independent by design: a fresh reader of the opposite family, sharing nothing with the writer's session; its re-verifications continue this worker");
let verdict = await send(verifier, verifyPrompt(null, null), { schema: findingsSchema, label: "verify" });
if (!verdict) throw new Error("verifier stage returned nothing usable — re-run, never read as clean");
if (!verdict.examined || !verdict.examined.trim())
  throw new Error("verifier examined-account empty — invalid report, re-run");

// ------------------------------------------------------- fix rounds + triage
phase("triage");

// Severe is the terminal severity: converge a severe-bearing verdict
// through autonomous fix->re-verify rounds before the first hold; a
// blocked fix or a severe finding surviving its round hands over early.
const autonomousRoundLimit = 3;
const totalRoundLimit = 5;
const severesIn = (v) => v.findings.filter((f) => f.severity === "severe");

const autoFixSchema = {
  type: "object",
  additionalProperties: false,
  required: ["summary", "head_commit", "blocked"],
  properties: {
    summary: { type: "string" },
    head_commit: { type: "string" },
    blocked: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["finding", "why"],
        properties: { finding: { type: "integer" }, why: { type: "string" } },
      },
    },
  },
};

const autoFixPrompt = (round) => `The verifier raised findings against your documentation unit (${unitDir}/${round === 0 ? "verdict.md" : `verdict-refix-${round - 1}.md`}), at least one of them severe. No orchestrator triage has happened yet: every finding in that verdict is yours to fix. Your brief's standing constraints remain in force: work only in ${worktree}; never push, open PRs, or merge; documentation files only — never product code, never the agent-guidance file; the changelog stays untouched. A finding you cannot fix honestly as written — the demanded correction would write a claim the code contradicts, or the finding disputes the brief or a ruling rather than the docs — goes to blocked in your reply and in ${unitDir}/report.md, with why; fix the rest. Commit as you go. Then make the whole of ${unitDir}/report.md true about the current tree — correct every section the fixes dated, not just the sections a finding quoted.

Reply with JSON: summary (what you fixed, in two or three sentences), head_commit (the new head), blocked (the findings deliberately left unfixed, each by its 0-based index in the verdict, with why; empty array when none). Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one. A reply that is the bare JSON value alone also works.`;

let fixReport = null;
let rounds = 0;
const autonomousRounds = [];
let handover = null;

while (severesIn(verdict).length) {
  if (autonomousRounds.length >= autonomousRoundLimit || rounds >= totalRoundLimit) {
    handover = "autonomous round limit reached with severe findings open";
    break;
  }
  const writerIdle = await idleReading(writer);
  dispatching(`fix:${rounds}`, "continued", `a fix round after its own write: the worker holds which claims it changed and from what evidence, so the findings land on context (${writerIdle.phrase}; continued regardless — the context is worth more than the cache)`);
  const autoFix = await send(writer, autoFixPrompt(rounds), {
    schema: autoFixSchema, label: `fix:${rounds}`, timeoutMs: 3600000,
  });
  if (!autoFix) throw new Error("autonomous fix round returned nothing usable — re-run, never read as applied");
  fixReport = autoFix.summary;
  const verifierIdle = await idleReading(verifier);
  dispatching(`verify:refix:${rounds}`, "continued", `a re-verification after its own verdict: the reader re-examines the claims it derived against the fix delta (${verifierIdle.phrase}; continued regardless)`);
  verdict = await send(verifier, verifyPrompt(rounds, "autonomous"), {
    schema: findingsSchema, label: `verify:refix:${rounds}`,
  });
  if (!verdict) throw new Error("re-verification returned nothing usable — re-run, never read as clean");
  if (!verdict.examined || !verdict.examined.trim())
    throw new Error("re-verifier examined-account empty — invalid report, re-run");
  rounds++;
  autonomousRounds.push({
    round: rounds - 1,
    fixed: autoFix.summary,
    head: autoFix.head_commit,
    blocked: autoFix.blocked,
    remaining_findings: verdict.findings.length,
    remaining_severe: severesIn(verdict).length,
  });
  if (autoFix.blocked.length) {
    handover = "the fix round declared blocked findings — they need dispositions, not another round";
    break;
  }
  if (severesIn(verdict).some((f) => f.unresolved_prior === true)) {
    handover = "a severe finding survived its fix round — re-dispositioning beats re-fixing";
    break;
  }
}

let issuesFindings = [];
while (true) {
  const triage = await runHold(
    rounds === 0 ? "triage" : `triage${rounds}`,
    {
      findings: verdict.findings,
      examined: verdict.examined,
      rejected_concerns: verdict.rejected_concerns,
      verifier_confidence: `${verdict.confidence} — ${verdict.confidence_why}`,
      writer_report: `${unitDir}/report.md`,
      fix_rounds_used: `${rounds} of ${totalRoundLimit}`,
      autonomous_rounds: autonomousRounds,
      autonomous_note: handover || "no severe findings remain; a land may leave sub-severe findings undispositioned — they return in the envelope as issues_findings for the annexe issue log",
    },
    ["land", "fix", "park"]
  );
  if (triage.verdict === "park") return { outcome: "parked", sketch, writeReport, verdict, fixReport, autonomous_rounds: autonomousRounds };

  // Validate the same disposition contract for landing and repair. A land
  // carrying fixes requests repair first; only a later decision can land it.
  const dispositions = triage.dispositions === undefined ? [] : triage.dispositions;
  const seen = new Set();
  let invalid = !Array.isArray(dispositions);
  if (!invalid) {
    for (const d of dispositions) {
      if (!d || typeof d !== "object" || Array.isArray(d)
          || !Number.isInteger(d.finding) || d.finding < 0
          || d.finding >= verdict.findings.length || seen.has(d.finding)
          || !["fix", "contract-question", "accept-with-note"].includes(d.disposition)
          || (d.note !== undefined && typeof d.note !== "string")
          || (d.disposition === "accept-with-note" && !d.note?.trim())) {
        invalid = true;
        break;
      }
      seen.add(d.finding);
    }
  }
  if (invalid) {
    log("decision refused: dispositions need unique valid finding indices, known dispositions and a non-empty note for acceptance — re-holding");
    continue;
  }
  if (dispositions.some((d) => d.disposition === "contract-question")) {
    log("decision refused: a contract question requires park — re-holding");
    continue;
  }
  const fixList = dispositions.filter((d) => d.disposition === "fix");
  if (triage.verdict === "land") {
    if (verdict.findings.some((f, i) => f.severity === "severe" && !seen.has(i))) {
      log("land refused: every severe finding needs a disposition — re-holding");
      continue;
    }
    if (!fixList.length) {
      issuesFindings = verdict.findings.filter((f, i) => f.severity !== "severe" && !seen.has(i));
      break;
    }
    log("land deferred: requested fixes must be repaired and re-verified before a fresh landing decision");
  }
  if (!fixList.length) {
    log("fix refused: at least one disposition must request a fix — re-holding");
    continue;
  }
  // Check the budget before dispatch so every final verification reaches a hold.
  if (rounds >= totalRoundLimit) {
    log(`fix refused: the ${totalRoundLimit}-round budget is spent — accept the measured residue with reasons or park for a re-brief — re-holding`);
    continue;
  }
  if (triage.note && triage.note.trim()) rulings.push(triage.note.trim());
  for (const d of dispositions)
    if (d.note && d.note.trim())
      rulings.push(`on ${rounds === 0 ? "verdict.md" : `verdict-refix-${rounds - 1}.md`} finding index ${d.finding} (${d.disposition}): ${d.note.trim()}`);
  const writerIdle = await idleReading(writer);
  dispatching(`fix:${rounds}`, "continued", `a directed fix round after its own write: the dispositions name findings against claims this worker holds in context (${writerIdle.phrase}; continued regardless — the context is worth more than the cache)`);
  fixReport = await send(
    writer,
    `The verifier raised findings against your documentation unit (${unitDir}/${rounds === 0 ? "verdict.md" : `verdict-refix-${rounds - 1}.md`}). The orchestrator's dispositions, by finding index: ${JSON.stringify(fixList)} — with this note: "${triage.note || ""}". Your brief's standing constraints remain in force: work only in ${worktree}; never push, open PRs, or merge; documentation files only — never product code, never the agent-guidance file; the changelog stays untouched. Apply exactly the dispositions marked fix — no more (an accepted-with-note finding is not yours to fix). A disposition you cannot apply honestly as written — the demanded correction would write a claim the code contradicts — stays unapplied: report it as blocked in your reply and in ${unitDir}/report.md, with why. Commit as you go. Then make the whole of ${unitDir}/report.md true about the current tree — correct every section the fixes dated, not just the sections a finding quoted. Reply with a summary and the new head commit.`,
    { label: `fix:${rounds}`, timeoutMs: 3600000 }
  );
  const verifierIdle = await idleReading(verifier);
  dispatching(`verify:refix:${rounds}`, "continued", `a re-verification after its own verdict, over the delta the orchestrator's dispositions authorised (${verifierIdle.phrase}; continued regardless)`);
  verdict = await send(verifier, verifyPrompt(rounds, "directed"), {
    schema: findingsSchema, label: `verify:refix:${rounds}`,
  });
  if (!verdict) throw new Error("re-verification returned nothing usable — re-run, never read as clean");
  if (!verdict.examined || !verdict.examined.trim())
    throw new Error("re-verifier examined-account empty — invalid report, re-run");
  rounds++;
}

// ---------------------------------------------------------------- land
phase("land");

const landingSchema = {
  type: "object",
  additionalProperties: false,
  required: ["landed", "detail"],
  properties: {
    landed: { type: "boolean" },
    detail: { type: "string" },
    target_head: { type: "string" },
    blocked_on: { type: "string" },
  },
};

// Docs units land by direct merge under either policy — never as a PR. The
// autonomous PR-review system reviews code PRs only, so a docs PR would wait
// on a review that never comes; this pipeline's cross-family accuracy audit
// and the triage hold are the unit's review of record.
const landTarget =
  mergePolicy === "direct-merge" ? "the repository's main branch" : `the branch ${integrationBranch}`;

dispatching("land", "re-briefed", "a merge needs no memory of the writing or the audit — report.md carries what it needs, and a fresh worker cannot mistake a draft it remembers for what actually landed");
const landing = await agent(
  `Land the finished documentation unit by direct merge — open no PR. First read ${annexeConfigPath} for push_policy. The unit branch is checked out in ${worktree}; note its head sha, then merge that head into ${landTarget}. Perform the merge in a checkout of the target branch: the repository worktree where it is already checked out, or a temporary worktree you create (git worktree add) and remove afterwards — never rewrite the target ref without a checkout. A conflicted merge is yours to resolve: keep both sides' content, then run the project's full gate on the resolved tree; the resolution stands only when it is green. Report as blocked ({landed:false, blocked_on:"conflict", detail: what contradicts what}) only a genuine contradiction the sides cannot both keep, or a resolution whose gate stays red; abort the merge before reporting. A fast-forward is a normal success. Push the target branch only if push_policy is "push-on-land"; under "hold-for-user" the merge stays local and detail says so. Reply with {landed, detail, target_head} — landed true only if the unit head is an ancestor of the target branch head (verify with git merge-base --is-ancestor). Reply with the JSON last.`,
  { model: "gpt-6.1-sol", schema: landingSchema, cwd: worktree, label: "land", timeoutMs: 1800000 }
);

// Closed on the success path so the journal records intent; a run ending
// by any other path closes every worker it still holds.
await close(writer);
await close(verifier);

// issues_findings — sub-severe findings accepted by default at the land —
// are the orchestrator's to file to the annexe issue log.
return { outcome: landing && landing.landed ? "landed" : "landing-blocked", sketch, writeReport, verdict, fixReport, autonomous_rounds: autonomousRounds, issues_findings: issuesFindings, landing };
