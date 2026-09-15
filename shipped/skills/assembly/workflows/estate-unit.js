// estate-unit — the assembly skill's estate pipeline: a builder delivers
// end-to-end tests for a ledger obligation, and a cross-family verifier
// independently demonstrates the estate property — every new test fails
// against the baseline behaviour and passes against the current tree —
// with severe findings converged by autonomous fix rounds before the
// triage hold. Acceptance is inverted: the tests are the deliverable.

export const defaults = {
  // Builders ride the flagship GPT tier at low effort. The builder is one
  // held worker — restatement, build, fix rounds and a warm landing are
  // turns of a single session — and a held worker's effort is fixed when it
  // opens, so fix rounds run at the build pin: the context the worker
  // carries into a fix round substitutes for the search a higher effort
  // would buy, and escalation is a park and a fresh re-brief. Claude legs
  // are the cross-family verifiers, pinned per call; the verifier is held
  // too, so every re-verification returns to the reader that ran the
  // estate demonstration.
  model: "gpt-5.6-sol",
  effort: "low",
};

// INPUT (workflow args): briefPath, worktree, commissionPath,
// verificationPath, unitDir, mergePolicy, integrationBranch (PR mode: the
// structural branch unit PRs target; required when mergePolicy is "pr"),
// annexeConfigPath, plus:
//   proofMode: "baseline" (default) | "mutation".
//   baselineRef: required in baseline mode — the ref WITHOUT the behaviour
//     under proof. The fail-before check runs the NEW tests (from the unit
//     worktree) against a separate checkout of this ref — never by checking
//     the unit branch out to the baseline, which would remove the tests
//     along with the behaviour. In mutation mode (no reachable tree lacks
//     the behaviour) baselineRef is omitted: fail-before is demonstrated by
//     deliberately breaking the behaviour in a scratch checkout of the
//     CURRENT tree, watching the tests fail, and restoring.
//
// Hold protocol: identical to build-verify.js — a "decision.needed" event
// carrying the hold's exact content, answered by a nonce-echoing
// "decision.answer" event posted from outside the script (lead segment or
// operator). No hold timeout: an unanswered decision blocks.

const {
  briefPath, worktree, commissionPath, verificationPath,
  unitDir, mergePolicy, annexeConfigPath, integrationBranch, baselineRef,
} = args;
if (mergePolicy !== "direct-merge" && mergePolicy !== "pr")
  throw new Error(`unknown mergePolicy: ${JSON.stringify(mergePolicy)} (expected "direct-merge" or "pr")`);
if (mergePolicy === "pr" && !integrationBranch)
  throw new Error('mergePolicy "pr" requires integrationBranch (the structural branch unit PRs target)');

const proofMode = args.proofMode === "mutation" ? "mutation" : "baseline";
if (proofMode === "baseline" && !baselineRef)
  throw new Error("estate-unit: baseline proof mode needs baselineRef; pass proofMode:'mutation' when no reachable tree lacks the behaviour");
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

// ---------------------------------------------------------------- restate
phase("restate");

const restatementSchema = {
  type: "object",
  additionalProperties: false,
  required: ["admission", "restatement", "journeys", "disputes"],
  properties: {
    admission: { type: "string", enum: ["ok", "refused"] },
    restatement: { type: "string" },
    journeys: { type: "string" },
    disputes: { type: "string" },
  },
};

const restatePrompt = (correction) => `Read the brief at ${briefPath}. FIRST, the admission check: the brief must carry an "Obligation:" line; if it is missing, set admission to "refused" and put the reason in disputes — do not restate a brief that fails admission. Then read the consumer ladder at ${verificationPath}. This is an estate unit: the deliverable is end-to-end tests proving the brief's obligation at the real consumer, not product code. Do not start work.${correction ? `\n\nYour earlier plan was corrected by the orchestrator: "${correction}". Apply it; restate the CHANGED part fully.` : ""}

Reply with: admission ("ok"/"refused"), restatement (the obligation in your own words), journeys (the concrete journey list you intend to cover, each naming the ladder rung it exercises), disputes ("" if none). Reply with the JSON last.`;

// The builder is one held worker from restatement onward: the journeys it
// planned against the ladder are the context the build turn starts from.
const builder = await session({ cwd: worktree, label: "builder" });
let plan = null;
let hold = null;
// Hold-decision notes — restate and triage alike — carried forward to every
// later verification: a ruling may supersede write-once brief text.
const rulings = [];
for (let attempt = 0; attempt < 3; attempt++) {
  dispatching(
    `restate:${attempt}`,
    attempt === 0 ? "re-briefed" : "continued",
    attempt === 0
      ? "the builder's held session opens on the brief; the build and its fix rounds continue this worker"
      : "the orchestrator corrected this worker's own plan — it refines the journeys it proposed rather than re-deriving them"
  );
  plan = await send(builder, restatePrompt(hold && hold.note), {
    schema: restatementSchema, label: `restate:${attempt}`,
  });
  if (!plan) throw new Error("restatement stage returned nothing usable");
  hold = await runHold("restate", plan, ["accept", "correct", "kill"]);
  if (hold.verdict === "kill") return { outcome: "killed-at-restatement", plan };
  if (hold.note && hold.note.trim()) rulings.push(hold.note.trim());
  if (hold.verdict === "accept") break;
}
if (hold.verdict !== "accept")
  return { outcome: "corrections-exhausted", at: "restate", salvage: { plan } };
if (plan.admission === "refused")
  return { outcome: "admission-refused", plan, note: "orchestrator accepted a refused admission — fix the brief's Obligation line" };

// ---------------------------------------------------------------- build
phase("build");

dispatching("build", "continued", "the build starts from the plan this worker made: the ladder rungs and journeys it chose are already in its context");
const report = await send(
  builder,
  `Build the estate this accepted plan describes — your own plan, as the orchestrator accepted it:

<accepted-plan>
${JSON.stringify(plan)}
</accepted-plan>

End-to-end tests for the listed journeys, exercising the real consumer per ${verificationPath} — never fixtures standing in for a rung the ladder names live; where the environment cannot drive that consumer at all, report the rung as unavailable in ${unitDir}/report.md and in your reply (what you tried, what stands in the way), prove the journeys you can, and leave the rung's fate to the orchestrator rather than substituting a stand-in. Work only in ${worktree}; commit as you go. Publication is the landing stage's, never yours: never push, never open PRs, never merge this unit's work outward — but merging ${mergePolicy === "pr" ? integrationBranch : "the repository's main branch"} into your own branch is ordinary maintenance, permitted when the unit's work needs it.

The estate property is the acceptance bar, and its mechanics matter. ${proofMode === "baseline"
  ? `To show a test FAILS against the baseline, create a separate checkout of ${baselineRef} (git worktree add <tmp> ${baselineRef}), copy the NEW test files into it, and run them there — never check the baseline out in your own worktree, which would delete the tests you are proving. Each new journey test must fail in that baseline checkout and pass in ${worktree}.`
  : `No reachable tree lacks this behaviour, so fail-before is proved by MUTATION: in a scratch checkout of the current tree (git worktree add <tmp> HEAD), deliberately break the behaviour under proof (name the exact break in the report), run the new tests there and watch them fail, then discard the scratch checkout. Each new journey test must fail under the mutation and pass in ${worktree}.`
} Record the exact commands and their output for both directions in ${unitDir}/report.md, alongside what you did not cover and your confidence. Remove the temporary checkout when done. Reply with a two-sentence summary and the head commit.`,
  { label: "build", timeoutMs: 14400000 }
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

const verifyPrompt = (refixRound, fixMode) => `You are the cross-family verifier for an estate unit. Judge against the brief, the accepted plan, and the orchestrator's rulings below — the latter two hold the brief's standing, and a conflicting ruling supersedes it. Rulings are listed in the order issued; where two conflict, the later governs. A note that merely records process rather than directing the work is context, not a constraint. Scrutinise a ruling's stated reasoning rather than re-deriving its question.
${rulings.length ? `\n<orchestrator-rulings>\n${rulings.map((r, i) => `${i + 1}. ${r}`).join("\n")}\n</orchestrator-rulings>\n` : ""}
<accepted-plan>
${JSON.stringify(plan)}
</accepted-plan>

The claim under test: the new journey tests in ${worktree} prove their obligation (brief: ${briefPath}) at the real consumer per the ladder (${verificationPath}), and each ${proofMode === "baseline" ? `FAILS against ${baselineRef}` : "FAILS under a deliberate break of the behaviour (mutation mode — the report names the break)"} while PASSING against the current tree.

Demonstrate the estate property yourself — ${proofMode === "baseline"
  ? `create your own checkout of ${baselineRef} (git worktree add <tmp> ${baselineRef}), copy the new test files in, run them there, then run them in ${worktree}`
  : `create your own scratch checkout of the current tree, apply the report's named break yourself (or an equivalent one), run the new tests there and confirm they fail, then run them in ${worktree}`}; do not take the report's word (${unitDir}/report.md — read only after your own runs). Clean up your checkout. Then judge the tests semantically: do they exercise the consumer the ladder names or a stand-in; do they prove the obligation or restate the implementation; is any test unable to fail. A test satisfiable by a broken product is a severe finding. Where the report declares a rung unavailable, judge the declaration on its evidence and surface it as its own finding for the orchestrator's disposition — an honest, well-evidenced unavailability is not under-delivery. Judge only whether these tests prove this obligation — missing coverage beyond the brief's obligation, extra journeys worth testing, or hardening the product code are not findings; proposing them is overbuild on your side of the fence.${refixRound !== null ? ` This is a RE-VERIFICATION after a fix round. ${fixMode === "autonomous" ? "The round ran autonomously — no orchestrator dispositions exist yet: every finding of the earlier verdict was dispatched for fix, and the fixer could leave one unfixed only by declaring it blocked in the report." : 'The orchestrator dispositioned the earlier findings; only those marked "fix" were dispatched.'} Focus on each finding dispatched for fix (${unitDir}/${refixRound === 0 ? "verdict.md" : `verdict-refix-${refixRound - 1}.md`}) — setting unresolved_prior true on any finding you now report that restates one the fix round failed to resolve — on anything the fix delta introduced, and on any fix the report declares deliberately left unapplied as blocked — judge its grounds and surface it as its own finding for the orchestrator's disposition, not as a failed fix.` : ""}

Every command your verdict leans on is evidenced by the command line as executed plus enough of its actual output to support the claim — or an explicit "could not run: <reason>"; a run cited with neither is an unevidenced claim, not a completed check. Report to the findings contract (examined / findings with severity+confidence+evidence, each carrying unresolved_prior — true only when it restates a finding from the earlier verdict that its fix round failed to resolve, false otherwise, and always false on a first verification / rejected_concerns / confidence + why), and write it to ${unitDir}/${refixRound !== null ? `verdict-refix-${refixRound}.md` : "verdict.md"}.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show a sample answer; the only JSON value in your reply is your actual answer.`;

// The verifier is a held worker of the opposite family: opened blind to the
// builder's session, then continued for every re-verification, so the
// estate demonstration it ran is what it re-runs against the fix delta.
const verifier = await session({
  model: "claude-opus-5", effort: "medium", cwd: worktree, label: "verifier",
});
dispatching("verify", "re-briefed", "cross-family verification is independent by design: a fresh reader of the opposite family, sharing nothing with the builder's session; its re-verifications continue this worker");
let verdict = await send(verifier, verifyPrompt(null, null), { schema: findingsSchema, label: "verify" });
if (!verdict) throw new Error("verifier stage returned nothing usable — re-run, never read as clean");
if (!verdict.examined || !verdict.examined.trim())
  throw new Error("verifier examined-account empty — invalid, re-run");

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

const autoFixPrompt = (round) => `The verifier raised findings (${unitDir}/${round === 0 ? "verdict.md" : `verdict-refix-${round - 1}.md`}), at least one of them severe. No orchestrator triage has happened yet: every finding in that verdict is yours to fix. Your brief's standing constraints remain in force: work only in ${worktree}; never push, open PRs, or merge; tests exercise the ladder's rung, never a stand-in. A finding you cannot fix honestly as written — it would need manufactured state or an untrue assertion, or it disputes the brief or a ruling rather than the work — goes to blocked in your reply and in ${unitDir}/report.md, with why; fix the rest. Re-demonstrate the estate property both directions (the same proof-mode mechanics as before — baseline checkout, or mutation), then make the whole of ${unitDir}/report.md true about the current tree — not just the lines a finding quoted.

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
  const builderIdle = await idleReading(builder);
  dispatching(`fix:${rounds}`, "continued", `a fix round after its own build: the worker holds the journeys it wrote and how it demonstrated them, so the findings land on context (${builderIdle.phrase}; continued regardless — the context is worth more than the cache)`);
  const autoFix = await send(builder, autoFixPrompt(rounds), {
    schema: autoFixSchema, label: `fix:${rounds}`, timeoutMs: 7200000,
  });
  if (!autoFix) throw new Error("autonomous fix round returned nothing usable — re-run, never read as applied");
  fixReport = autoFix.summary;
  const verifierIdle = await idleReading(verifier);
  dispatching(`verify:refix:${rounds}`, "continued", `a re-verification after its own verdict: the reader repeats its estate demonstration over the fix delta against what it already saw (${verifierIdle.phrase}; continued regardless)`);
  verdict = await send(verifier, verifyPrompt(rounds, "autonomous"), {
    schema: findingsSchema, label: `verify:refix:${rounds}`,
  });
  if (!verdict) throw new Error("re-verification returned nothing usable");
  if (!verdict.examined || !verdict.examined.trim())
    throw new Error("re-verifier examined-account empty — invalid, re-run");
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
  const triage = await runHold(rounds === 0 ? "triage" : `triage${rounds}`, {
    findings: verdict.findings,
    examined: verdict.examined,
    rejected_concerns: verdict.rejected_concerns,
    verifier_confidence: `${verdict.confidence} — ${verdict.confidence_why}`,
    builder_report: `${unitDir}/report.md`,
    fix_rounds_used: `${rounds} of ${totalRoundLimit}`,
    autonomous_rounds: autonomousRounds,
    autonomous_note: handover || "no severe findings remain; a land may leave sub-severe findings undispositioned — they return in the envelope as issues_findings for the annexe issue log",
  }, ["land", "fix", "park"]);
  if (triage.verdict === "park") return { outcome: "parked", plan, report, verdict, fixReport, autonomous_rounds: autonomousRounds };

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
  // A ruling issued at triage stays in force for every later verification:
  // the re-verifier judges the delta the ruling authorised, so a triage note
  // it never saw makes a compliant builder read as ignoring the brief.
  if (triage.note && triage.note.trim()) rulings.push(triage.note.trim());
  for (const d of dispositions)
    if (d.note && d.note.trim())
      rulings.push(`on ${rounds === 0 ? "verdict.md" : `verdict-refix-${rounds - 1}.md`} finding index ${d.finding} (${d.disposition}): ${d.note.trim()}`);
  const builderIdle = await idleReading(builder);
  dispatching(`fix:${rounds}`, "continued", `a directed fix round after its own build: the dispositions name findings against tests this worker holds in context (${builderIdle.phrase}; continued regardless — the context is worth more than the cache)`);
  fixReport = await send(
    builder,
    `The verifier raised findings (${unitDir}/${rounds === 0 ? "verdict.md" : `verdict-refix-${rounds - 1}.md`}); the orchestrator's fix dispositions by index: ${JSON.stringify(fixList)}, note: "${triage.note || ""}". Your brief's standing constraints remain in force: work only in ${worktree}; never push, open PRs, or merge; tests exercise the ladder's rung, never a stand-in. Apply exactly those dispositions — no more. A disposition you cannot apply honestly as written — it would need manufactured state or an untrue assertion — stays unapplied: report it as blocked in your reply and in ${unitDir}/report.md, with why. Re-demonstrate the estate property both directions (the same proof-mode mechanics as before — baseline checkout, or mutation), then make the whole of ${unitDir}/report.md true about the current tree — not just the lines a finding quoted. Reply with a summary and the new head commit.`,
    { label: `fix:${rounds}`, timeoutMs: 7200000 }
  );
  const verifierIdle = await idleReading(verifier);
  dispatching(`verify:refix:${rounds}`, "continued", `a re-verification after its own verdict, over the delta the orchestrator's dispositions authorised (${verifierIdle.phrase}; continued regardless)`);
  verdict = await send(verifier, verifyPrompt(rounds, "directed"), {
    schema: findingsSchema, label: `verify:refix:${rounds}`,
  });
  if (!verdict) throw new Error("re-verification returned nothing usable");
  if (!verdict.examined || !verdict.examined.trim())
    throw new Error("re-verifier examined-account empty — invalid, re-run");
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
    merge_commit: { type: "string" },
    pr_number: { type: "integer" },
    pr_url: { type: "string" },
    blocked_on: { type: "string" },
  },
};

const landPrompt =
  mergePolicy === "direct-merge"
    ? `Land the finished estate unit. Read ${annexeConfigPath} for push_policy first. From ${worktree}, merge the unit branch into main. A conflicted merge is yours to resolve: keep both sides' behaviour, then run the project's full gate on the resolved tree; the resolution stands only when it is green. Report as blocked ({landed:false, blocked_on:"conflict", detail: what contradicts what}) only a genuine semantic contradiction the sides cannot both keep, or a resolution whose gate stays red; abort the merge before reporting. Push main only under push_policy "push-on-land". Reply {landed, detail, merge_commit}, landed true only if the merge commit exists on main (verify with git log). JSON last.`
    : `Publish the finished estate unit as a blocked PR. Read ${annexeConfigPath} for push_policy first: under "hold-for-user" do NOT push — reply {landed:false, blocked_on:"push_policy hold-for-user", detail: why the push is held}. Otherwise write ${unitDir}/pr.md whole from the report (What changed / Why / What must be true / Known gaps), push the branch, open the PR "WIP:"-prefixed via the forge API, base ${integrationBranch} (the unit's integration lane — the structural branch for lane units, whose promotion PR carries them to main later, or main itself for a Lane: main unit) (tokens under ~/.config/forgejo/; refs/for/* AGit pushes forbidden; auth failure → {landed:false, blocked_on:"forge auth", detail: what failed}). Confirm by API read that the PR exists with a non-empty body; landed true only on that confirmation. Reply {landed, detail, pr_number, pr_url}. JSON last.`;

// The landing continues the builder while its session is warm and re-briefs
// a fresh worker once the triage hold has outlived the route's cache: the
// landing is mechanical, and report.md carries what a fresh worker needs.
// The reading decides; the reason records it.
const builderIdle = await idleReading(builder);
let landing;
if (builderIdle.stale === false) {
  dispatching("land", "continued", `the builder publishes its own unit while its session is warm (${builderIdle.phrase})`);
  landing = await send(builder, landPrompt, { schema: landingSchema, label: "land", timeoutMs: 1800000 });
} else {
  dispatching("land", "re-briefed", `the landing is mechanical and ${unitDir}/report.md carries what it needs; ${builderIdle.stale === true ? "the builder's session is cold" : "the route declares no staleness threshold, so warmth is unknown"} (${builderIdle.phrase})`);
  landing = await agent(landPrompt, { schema: landingSchema, cwd: worktree, label: "land", timeoutMs: 1800000 });
}
// Closed on the success path so the journal records intent; a run ending
// by any other path closes every worker it still holds.
await close(builder);
await close(verifier);

// issues_findings — sub-severe findings accepted by default at the land —
// are the orchestrator's to file to the annexe issue log.
return { outcome: landing && landing.landed ? "landed" : "landing-blocked", plan, report, verdict, fixReport, autonomous_rounds: autonomousRounds, issues_findings: issuesFindings, landing };
