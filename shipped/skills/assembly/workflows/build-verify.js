// build-verify — the assembly skill's standard unit pipeline:
// admission-checked restatement (hold), build, cross-family semantic
// verification, autonomous fix rounds that converge severe findings before
// the orchestrator holds, per-finding triage of the residue (hold), then a
// landing stage that reports a derived result. Verification is a stage,
// not a decision.

export const defaults = {
  // Builders ride the flagship GPT tier at low effort: the brief plus the
  // named exemplar have already decided the path, so the tier carries the
  // capability and effort stays the cost lever. Fix rounds override effort
  // to medium — a verdict that sent the unit back falsifies the pin's
  // ground that the path is decided. Claude legs are the
  // cross-family verifiers, deliberately below the top tier (the top tier
  // appears nowhere in assembly pipelines), pinned per call. Pinned because
  // per-model effort defaults are uneven.
  model: "gpt-5.6-sol",
  effort: "low",
};

// INPUT (workflow args):
//   briefPath, worktree, commissionPath, spinePath, verificationPath,
//   unitDir, mergePolicy ("direct-merge"|"pr"), integrationBranch (PR mode:
//   the branch this unit's PR targets — the structural branch, or the parent
//   unit's branch for a stacked unit; required when mergePolicy is "pr"),
//   annexeConfigPath
//   (config.toml, for push_policy)
//
// HOLD PROTOCOL (documented for the orchestrator in references/pipelines.md):
// at each hold the pipeline posts a "decision-needed" event carrying the
// hold's exact content and pauses on the run's event queue until the
// answering "decision" event arrives — posted from outside the script by a
// lead segment clearing its batch of decisions at a wake, or by the
// operator through the run surface. The decision's payload echoes the
// hold's nonce and carries {verdict, note?, dispositions?}; a stale
// decision from an earlier attempt names a different nonce and is
// ignored — the script checks the echoed nonce itself. There is no hold
// timeout: an unanswered decision blocks, and the parked run's journal
// carries every completed stage.

const {
  briefPath, worktree, commissionPath, spinePath, verificationPath,
  unitDir, mergePolicy, annexeConfigPath, integrationBranch,
} = args;
if (mergePolicy !== "direct-merge" && mergePolicy !== "pr")
  throw new Error(`unknown mergePolicy: ${JSON.stringify(mergePolicy)} (expected "direct-merge" or "pr")`);
if (mergePolicy === "pr" && !integrationBranch)
  throw new Error('mergePolicy "pr" requires integrationBranch (the structural branch unit PRs target)');

// One nonce PER HOLD, minted inside runHold from a deterministic counter
// (the sealed context has no clock or randomness, and determinism is what
// replay needs): reusing a nonce across a correction loop would let a
// previous attempt's stale decision satisfy the fresh hold the loop exists
// to require.
let holdCount = 0;
const mintNonce = (stage) => `${stage}-${++holdCount}`;

// The decision event's payload contract, validated by the script itself:
//   { nonce, verdict: one of the offered options,
//     note?: string,
//     dispositions?: [{ finding: int, disposition: "fix" |
//       "contract-question" | "accept-with-note", note?: string }] }

const runHold = async (stage, content, verdicts) => {
  const nonce = mintNonce(stage);
  // Raise the decision on the run's event queue and park until answered —
  // by a lead segment clearing its batch of decisions at a wake, or by the
  // operator through the run surface. Blocking is accepted: there is no
  // hold timeout, and the journal carries every completed stage meanwhile.
  postEvent("decision-needed", { stage, nonce, content, options: verdicts });
  while (true) {
    const decision = await awaitEvent({ kinds: ["decision"] });
    const payload = decision && decision.payload;
    if (!payload || payload.nonce !== nonce) {
      log(`[hold] ${stage}: stale or unaddressed decision ignored; still waiting for ${nonce}`);
      continue;
    }
    if (!verdicts.includes(payload.verdict)) {
      log(`[hold] ${stage}: unknown verdict ${JSON.stringify(payload.verdict)} ignored (offered: ${verdicts.join(", ")}); still waiting for ${nonce}`);
      continue;
    }
    return payload;
  }
};

// ---------------------------------------------------------------- restate
phase("restate");

const restatementSchema = {
  type: "object",
  additionalProperties: false,
  required: ["admission", "restatement", "approach", "disputes"],
  properties: {
    admission: { type: "string", enum: ["ok", "refused"] },
    restatement: { type: "string" },
    approach: { type: "string" },
    disputes: { type: "string" },
  },
};

const restatePrompt = (correction) => `Read the brief at ${briefPath}. FIRST, the admission check: the brief must carry an "Obligation:" line and an "Extends:" line, and the named exemplar must have an entry in ${spinePath}. If either line is missing, or the exemplar is absent from the inventory, or the work the brief describes plainly falls outside that exemplar's declared envelope (its "Extends at" scope, or past its "Imitation stops at" boundary), set admission to "refused" and put the reason in disputes — do not restate a brief that fails admission.

If admission passes: read the exemplar's spine entry and its landed files, and the commission slice the brief's intent cites (${commissionPath}). Do not start work.${correction ? `\n\nYour earlier restatement was corrected by the orchestrator: "${correction}". Apply the correction; restate the CHANGED part fully.` : ""}

Reply with: admission ("ok"/"refused"), restatement (the brief in your own words — outcome and constraints, not a paraphrase of its sentences), approach (the concrete first moves, what you extend where, your test plan), disputes (anything the brief has wrong — the exemplar does not cover this, the obligation is already true, a constraint contradicts the code; "" if none). Reply with the JSON last.`;

let sketch = null;
let holdDecision = null;
// Hold-decision notes — restate and triage alike — carried forward to every
// later verification: a ruling may supersede write-once brief text.
const rulings = [];
for (let attempt = 0; attempt < 3; attempt++) {
  sketch = await agent(restatePrompt(holdDecision && holdDecision.note), {
    schema: restatementSchema,
    cwd: worktree,
    label: `restate:${attempt}`,
    phase: "restate",
  });
  if (!sketch) throw new Error("restatement stage returned nothing usable");
  // Admission refusal and disputes go to the orchestrator like any sketch —
  // the hold is where the adjudicating judgement enters, corrected or not.
  holdDecision = await runHold("restate", sketch, ["accept", "correct", "kill"]);
  if (holdDecision.verdict === "kill") return { outcome: "killed-at-restatement", sketch };
  if (holdDecision.note && holdDecision.note.trim()) rulings.push(holdDecision.note.trim());
  if (holdDecision.verdict === "accept") break;
  // "correct" loops: the corrected restatement returns to a fresh hold —
  // a correction the orchestrator never re-reads is a correction unverified.
}
if (holdDecision.verdict !== "accept")
  return { outcome: "corrections-exhausted", at: "restate", salvage: { sketch } };
if (sketch.admission === "refused")
  return { outcome: "admission-refused", sketch, note: "orchestrator accepted a refused admission — route the obligation needs-spine or fix the brief" };

// ---------------------------------------------------------------- build
phase("build");

// The accepted sketch is persisted so the builder builds what was accepted,
// not what it happens to re-derive: fresh agents share no memory.
const buildReport = await agent(
  `Build the unit this accepted restatement describes (persist trust in it — it was accepted by the orchestrator over the brief at ${briefPath}):

<accepted-restatement>
${JSON.stringify(sketch)}
</accepted-restatement>

Work only in this worktree (${worktree}); commit as you go with plain descriptive messages. Publication is the landing stage's, never yours: never push, never open PRs, never merge this unit's work outward — but merging ${mergePolicy === "pr" ? integrationBranch : "the repository's main branch"} into your own branch is ordinary maintenance, permitted when the unit's work needs it. Extend the named spine exemplar's pattern; its imitation boundary and envelope (its entry in ${spinePath}) are hard constraints — incidental wiring the extension requires (registration, imports, config, tests beside the code) is part of extending; modifying non-spine files beyond that wiring is not yours, report it as blocked instead. Verify your work within the unit's scope as the brief's proof expectation describes, then run the project's check gate and the tests covering the surfaces you touched, on the tree your build finishes on, and get them green — fix what they find and re-run as many times as that takes. The full test-suite gate is not yours: it runs later, batched over landed units. Every new test must discriminate — test something genuine that a broken product would fail, with a real failure the assertion can register, never a check the change's mere presence satisfies: write each test of behaviour this unit builds so it would fail without your change, and never let a fixture, seed, or mock itself supply the state the assertion checks; a test of behaviour that predates the unit proves its teeth by mutation instead — break the behaviour deliberately in a scratch copy, watch the test fail, restore. Demonstrating the failure is the preferred evidence because it is usually cheap; where the demonstration itself would need machinery out of proportion to the change — a separately triggered build estate, a reconstructed historical tree, a live external dependency, a bespoke process harness — do not build that machinery: state in your report why demonstration is disproportionate — covering both routes, the scratch-tree fail-before run and the mutation fallback (break the behaviour deliberately in a scratch copy, watch the test fail); mutation is usually cheap, so most would-be declarations fall to it — and what instead shows the test discriminates (the real state it runs over, the behaviour's own output its assertion reads, the specific failure a broken product would produce); its disposition is the orchestrator's. Where you cannot meet the brief's proof expectation honestly at its named rung — the surface never shows the state, or the product cannot produce it — do not manufacture the state: report the impossibility as a failure finding (what the rung cannot show, and the strongest honest proof you produced instead); its disposition is the orchestrator's. If you cannot get it green, say so in your report as a failure finding — what failed, what you tried — never as a quietly recorded red result. Later stages audit your final recorded run rather than repeat it.

Write ${unitDir}/report.md: what was built at one level above the diff; what was tried and abandoned, and why; what fought back; how you verified and what you did NOT exercise; the check gate's run and the touched-surface test runs — the exact commands, the tree sha they ran on, and the results; your overall confidence (high/medium/low) with a one-line why. Reply with a two-sentence summary and the head commit hash.`,
  { cwd: worktree, label: "build", phase: "build", timeoutMs: 14400000 }
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

const verifyPrompt = (refixRound, fixMode) => `You are the cross-family verifier for a built unit. Judge against the brief at ${briefPath}, the accepted restatement, and the orchestrator's rulings below — the latter two hold the brief's standing, and a conflicting ruling supersedes it: delivering what a ruling withheld is the finding, not complying with it. Rulings are listed in the order issued; where two conflict, the later governs. A note that merely records process rather than directing the work is context, not a constraint. Scrutinise a ruling's stated reasoning rather than re-deriving its question.
${rulings.length ? `\n<orchestrator-rulings>\n${rulings.map((r, i) => `${i + 1}. ${r}`).join("\n")}\n</orchestrator-rulings>\n` : ""}
<accepted-restatement>
${JSON.stringify(sketch)}
</accepted-restatement>

Derive your claims BEFORE reading the builder's report: read the brief, the commission slice its intent cites (${commissionPath}), and the consumer ladder at ${verificationPath} — check the brief's proof expectation against the ladder, honouring its two sanctioned outcomes — a claim narrower than the obligation's proved by a smaller proof at the same rung, and a below-ladder selection the brief declares as a degradation: the finding is an UNdeclared weaker rung, and where a declaration exists you judge its stated grounds, never the bare selection. Then examine the worktree at ${worktree} (the diff against its base, the code, the tests) and exercise what the proof expectation names; where your judgement needs a check run of its own, run the checks covering the unit's touched surfaces, not a project gate — the full gate's venues are the batched lane gate and the merged result's CI, the builder's venue records the check gate, and your value is the semantic judgement a gate run cannot give. Only after that, read ${unitDir}/report.md and audit its claims against what you saw — the recorded check-gate run among them (the exact command, the tree sha, the result): a record that is missing, red without a declared failure finding, claiming a tree this unit's branch never held, or contradicted by what you saw is a finding — report it like any other rather than quietly re-running the gate to patch it. The report may also carry sanctioned declarations — a proof expectation unmeetable at its rung, a fail-before demonstration declared disproportionate (judged on whether what it offers instead shows the test discriminates), or, after a fix round, a disposition left unapplied as blocked: judge each on its evidence and surface it as its own finding for the orchestrator's disposition — an honest, well-evidenced declaration is not under-delivery, and a hollow one is the ordinary defect it is. A record whose sha sits behind the head because fix commits followed it is expected, not a finding: the fix delta is verified scoped, and the lane gate covers the composed result.${refixRound !== null ? `\n\nThis is a RE-VERIFICATION after a fix round. ${fixMode === "autonomous" ? "The round ran autonomously — no orchestrator dispositions exist yet: every finding of the earlier verdict was dispatched for fix, and the fixer could leave one unfixed only by declaring it blocked in the report." : 'The orchestrator dispositioned the earlier findings; only those marked "fix" were dispatched.'} Focus on: (1) each finding dispatched for fix — is it genuinely resolved, with the fix commits as evidence — setting unresolved_prior true on any finding you now report that restates one the fix round failed to resolve; (2) whether the fix delta introduced anything new, re-examining what your earlier verdict said it examined; (3) every test whose assertions, fixtures, seeds, mocks, or harness the fix touched, judged on whether it could still fail — the fix was written for the case that just failed, which is where the next blind spot forms; (4) any fix the report declares deliberately left unapplied as blocked — judge its grounds and surface it as its own finding for the orchestrator's disposition, not as a failed fix. Earlier verdict: ${unitDir}/${refixRound === 0 ? "verdict.md" : `verdict-refix-${refixRound - 1}.md`}.` : ""}

Falsifiability is its own check: judge every test the unit adds or changes on whether it could fail. Read the state each runs over — fixture, seed, mock — against its claim: a seed that itself satisfies the assertion, or a mock that never reaches the code under test, leaves the test passing with the behaviour absent. Read each assertion's shape against its claim: a presence check (an any-match, a contains, a membership test) carrying a structural claim about order, cardinality, or destination proves less than it reads — name the case the test never looks at (that naming judges the brief's own proof, not extra coverage: the overbuild fence below does not apply to it). Then demonstrate fail-before for the test carrying the proof expectation's discriminating case — where the brief names none, derive the case from the obligation and say in examined which test you judged to carry it: in a scratch worktree outside ${worktree} (git worktree add <tmp> <base>; remove it when done), check out the base the unit's diff is judged against (the merge-base of the unit branch with ${mergePolicy === "pr" ? integrationBranch : "the repository's main branch"}), carry the unit's new tests and their support files across, run that test there, and confirm it fails for the behaviour's absence. A failure to compile or load is not that confirmation — fall back to mutation: break the built behaviour deliberately in a scratch copy of the unit tree and watch the test fail. Where the builder's report declares the demonstration disproportionate, judge the declaration on the substitute discrimination evidence it offers, never by re-demanding the machinery it declined to build. Where you forgo both demonstration routes yourself, say in examined what stopped each and what substitute evidence supports any non-finding you rest on that gap — the bar is that the test discriminates, and what stopped you is not, alone, evidence that it does. After a fix round, repeat the demonstration only where the fix touched the discriminating test or the state it runs over. A test satisfiable by a broken product is a severe finding, however the demonstration question resolves.

Duplication is a standing question, and it is a second-occurrence test: take the helper, scaffold, and type names and distinctive signatures the diff introduces, and search the tree for an existing implementation — a grep is enough. A match is an implementation already in the tree, never the definition this diff itself introduces. Fetch the integration lane's tip first (one git fetch — cheap), so what siblings have landed on ${mergePolicy === "pr" ? integrationBranch : "the repository's main branch"} since this branch was cut is visible; siblings still in flight are invisible, and that gap is accepted. A match is a finding, never silent acceptance (it judges repetition already in the tree, not extra robustness: the overbuild fence below does not apply to it), and it is at most moderate — duplication never fuels severe convergence. Its evidence names the disposition it fits: an existing implementation the unit can trivially adopt (use it, delete the copy — an ordinary fix), or one whose adoption needs a cross-unit extraction — say so, because that extraction rides the sub-severe issues_findings route to the annexe issue log for later work, never grown into this unit's diff.

Judge semantically: does the built thing fit the brief; does it fit the commission; does it stay inside the ask — work beyond the brief's outcome is a finding (overbuild), not a bonus. Where the repository keeps a changelog and the unit's changes are ones a user of the project would notice, check the changelog entry: an entry that is missing, or that does not match what the diff actually changes, is a finding (the brief's changelog expectation states when the unit is internal-only). The same boundary binds your own findings: judge whether the unit does what the brief says and proves it at the named rung, never whether it could be more robust, general, or complete than the brief asks — a hardening or capability suggestion beyond the brief is not a finding, it is you overbuilding. Brief-versus-report divergence is a finding whatever the tests show. Hunt for why the unit is wrong, not for reasons to agree.

Every command your verdict leans on is evidenced by the command line as executed plus enough of its actual output to support the claim — or an explicit "could not run: <reason>"; a run cited with neither is an unevidenced claim, not a completed check. Report to the findings contract: examined (what you read, ran, and exercised — and what you could not), findings (each with severity, confidence, a verbatim evidence quote or command output, and unresolved_prior — true only when it restates a finding from the earlier verdict that its fix round failed to resolve, false otherwise, and always false on a first verification), rejected_concerns, confidence in your own read (bounded by the unseen list) and its one-line why. Write the same content to ${unitDir}/${refixRound !== null ? `verdict-refix-${refixRound}.md` : "verdict.md"}.

Reply with a single JSON value conforming to the schema. Reason first if you need to, but the JSON value comes last: the final non-whitespace characters of your message must be the JSON value itself — no prose, no code fence, no example after it. Never restate the schema or show a sample answer; the only JSON value in your reply is your actual answer.`;

let verdict = await agent(verifyPrompt(null, null), {
  model: "claude-opus-5", effort: "medium", schema: findingsSchema, cwd: worktree, label: "verify", phase: "verify",
});
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

const autoFixPrompt = (round) => `The verifier raised findings against your unit (${unitDir}/${round === 0 ? "verdict.md" : `verdict-refix-${round - 1}.md`}), at least one of them severe. No orchestrator triage has happened yet: every finding in that verdict is yours to fix.${rulings.length ? `\n\nThe orchestrator's rulings below hold the brief's standing, and a conflicting ruling supersedes it — delivering what a ruling withheld is a defect, not a fix. Where two conflict, the later governs.\n<orchestrator-rulings>\n${rulings.map((r, i) => `${i + 1}. ${r}`).join("\n")}\n</orchestrator-rulings>\n` : ""} Your brief's standing constraints remain in force: work only in ${worktree}; never push, open PRs, or merge the unit's work outward — publication is the landing stage's, though merging the integration lane into the unit branch stays permitted where the work needs it; the spine exemplar's imitation boundary still binds. A finding you cannot fix honestly as written — the demanded proof or change cannot be produced without manufacturing state or asserting something untrue, or the finding disputes the brief or a ruling rather than the work — goes to blocked in your reply and in ${unitDir}/report.md, with why. A duplication finding whose verdict routes it to filing (adoption needs a cross-unit extraction) is not yours either — leave it as it stands rather than growing the unit's diff. Fix the rest. Commit as you go. Verify each fix with the checks covering the surfaces it touched — no project gate re-runs per fix round; the lane gate covers the composed result. Then make the whole of ${unitDir}/report.md true about the current tree — re-derive what changed from git log over the unit's commits and correct every section the fixes dated (built, tried-and-abandoned, verification, confidence, any commit inventory), not just the sections a finding quoted.

Reply with JSON: summary (what you fixed, in two or three sentences), head_commit (the new head), blocked (the findings deliberately left unfixed, each by its 0-based index in the verdict, with why; empty array when none). Reason first if you need to, but the JSON value comes last: the final non-whitespace characters of your message must be the JSON value itself.`;

let fixReport = null;
let rounds = 0;
const autonomousRounds = [];
let handover = null;

while (severesIn(verdict).length) {
  if (autonomousRounds.length >= autonomousRoundLimit || rounds >= totalRoundLimit) {
    handover = "autonomous round limit reached with severe findings open";
    break;
  }
  const autoFix = await agent(autoFixPrompt(rounds), {
    effort: "medium", schema: autoFixSchema,
    cwd: worktree, label: `fix:${rounds}`, phase: "triage", timeoutMs: 7200000,
  });
  if (!autoFix) throw new Error("autonomous fix round returned nothing usable — re-run, never read as applied");
  fixReport = autoFix.summary;
  verdict = await agent(verifyPrompt(rounds, "autonomous"), {
    model: "claude-opus-5", effort: "medium", schema: findingsSchema, cwd: worktree, label: `verify:refix:${rounds}`, phase: "triage",
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
      builder_report: `${unitDir}/report.md`,
      fix_rounds_used: `${rounds} of ${totalRoundLimit}`,
      autonomous_rounds: autonomousRounds,
      autonomous_note: handover || "no severe findings remain; a land may leave sub-severe findings undispositioned — they return in the envelope as issues_findings for the annexe issue log",
    },
    ["land", "fix", "park"]
  );
  if (triage.verdict === "park") return { outcome: "parked", sketch, buildReport, verdict, fixReport, autonomous_rounds: autonomousRounds };

  if (triage.verdict === "land") {
    // Landing requires every SEVERE finding dispositioned exactly once with
    // a valid 0-based index, and no contract-question among the dispositions:
    // a contract question's documented result is a parked obligation, so it
    // cannot accompany a land verdict. Sub-severe findings left
    // undispositioned are accepted by default and returned as
    // issues_findings for the orchestrator to file to the annexe issue log —
    // so neglect can accept a minor into the issue log, never waive a severe.
    const ds = triage.dispositions || [];
    const seen = new Set();
    let invalid = false;
    for (const d of ds) {
      if (!Number.isInteger(d.finding) || d.finding < 0 || d.finding >= verdict.findings.length || seen.has(d.finding)) invalid = true;
      seen.add(d.finding);
    }
    const undisposedSevere = verdict.findings
      .map((f, i) => (f.severity === "severe" && !seen.has(i) ? i : null))
      .filter((i) => i !== null);
    const contractQs = ds.filter((d) => d.disposition === "contract-question");
    if (invalid || undisposedSevere.length || contractQs.length) {
      log(`land refused: ${invalid ? "invalid disposition index; " : ""}${undisposedSevere.length} severe finding(s) undispositioned; ${contractQs.length} contract-question(s) (those park the obligation) — re-holding`);
      continue;
    }
    issuesFindings = verdict.findings.filter((f, i) => f.severity !== "severe" && !seen.has(i));
    break;
  }

  // hold-directed fix round: apply exactly the dispositions marked fix, then
  // RE-VERIFY — a fix delta no verifier saw is the unverified-claim leak
  // reopened. The budget check runs before the dispatch, never after the
  // re-verify, so the final verdict always reaches a hold.
  if (rounds >= totalRoundLimit) {
    log(`fix refused: the ${totalRoundLimit}-round budget is spent — land or park on the verdict as it stands (further repair is a re-briefed unit) — re-holding`);
    continue;
  }
  // A fix verdict is also a disposition of every finding, and the structured
  // record must survive fix rounds as it survives lands: validate as the land
  // branch does. A decision carrying all its instruction in the prose note
  // alone leaves the rounds that shaped the unit with no disposition record.
  const fixDs = triage.dispositions || [];
  {
    const seen = new Set();
    let invalid = false;
    for (const d of fixDs) {
      if (!Number.isInteger(d.finding) || d.finding < 0 || d.finding >= verdict.findings.length || seen.has(d.finding)) invalid = true;
      seen.add(d.finding);
    }
    const fixCount = fixDs.filter((d) => d.disposition === "fix").length;
    const contractQs = fixDs.filter((d) => d.disposition === "contract-question").length;
    if (invalid || !fixDs.length || !fixCount || contractQs) {
      log(`fix refused: ${invalid ? "invalid disposition index; " : ""}${!fixDs.length ? "no dispositions; " : !fixCount ? 'no disposition is "fix" — a fix verdict with none dispatches an empty round; ' : ""}${contractQs ? `${contractQs} contract-question(s) — a contract question's documented result is a parked obligation, so answer with park or re-disposition, never a fix dispatch; ` : ""}a fix verdict dispositions its findings by 0-based index with at least one marked fix (the note carries reasoning, never the record) — re-holding`);
      continue;
    }
  }
  const fixList = fixDs.filter((d) => d.disposition === "fix");
  // A ruling issued at triage stays in force for every later verification:
  // the re-verifier judges the delta the ruling authorised, so a triage note
  // it never saw makes a compliant builder read as ignoring the brief.
  if (triage.note && triage.note.trim()) rulings.push(triage.note.trim());
  for (const d of fixDs)
    if (d.note && d.note.trim())
      rulings.push(`on ${rounds === 0 ? "verdict.md" : `verdict-refix-${rounds - 1}.md`} finding index ${d.finding} (${d.disposition}): ${d.note.trim()}`);
  fixReport = await agent(
    `The verifier raised findings against your unit (${unitDir}/${rounds === 0 ? "verdict.md" : `verdict-refix-${rounds - 1}.md`}). The orchestrator's dispositions, by finding index: ${JSON.stringify(fixList)} — with this note: "${triage.note || ""}".${rulings.length ? `\n\nThe orchestrator's rulings below hold the brief's standing, and a conflicting ruling supersedes it — delivering what a ruling withheld is a defect, not a fix. Where two conflict, the later governs.\n<orchestrator-rulings>\n${rulings.map((r, i) => `${i + 1}. ${r}`).join("\n")}\n</orchestrator-rulings>\n` : ""} Your brief's standing constraints remain in force: work only in ${worktree}; never push, open PRs, or merge the unit's work outward — publication is the landing stage's, though merging the integration lane into the unit branch stays permitted where the work needs it; the spine exemplar's imitation boundary still binds. Apply exactly the dispositions marked fix — no more (an accepted-with-note finding is not yours to fix). A disposition you cannot apply honestly as written — the demanded proof or change cannot be produced without manufacturing state or asserting something untrue — stays unapplied: report it as blocked in your reply and in ${unitDir}/report.md, with why. A duplication finding whose verdict routes it to filing (adoption needs a cross-unit extraction) is not yours to perform either — leave it as it stands rather than growing the unit's diff. Commit as you go. Verify each fix with the checks covering the surfaces it touched — no project gate re-runs per fix round; the lane gate covers the composed result. Then make the whole of ${unitDir}/report.md true about the current tree — re-derive what changed from git log over the unit's commits and correct every section the fixes dated (built, tried-and-abandoned, verification, confidence, any commit inventory), not just the sections a finding quoted. Reply with a summary and the new head commit.`,
    { effort: "medium", cwd: worktree, label: `fix:${rounds}`, phase: "triage", timeoutMs: 7200000 }
  );
  verdict = await agent(verifyPrompt(rounds, "directed"), {
    model: "claude-opus-5", effort: "medium", schema: findingsSchema, cwd: worktree, label: `verify:refix:${rounds}`, phase: "triage",
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
    merge_commit: { type: "string" },
    pr_number: { type: "integer" },
    pr_url: { type: "string" },
    blocked_on: { type: "string" },
  },
};

const landing = await agent(
  mergePolicy === "direct-merge"
    ? `Land the finished unit. First read ${annexeConfigPath} for push_policy. From ${worktree}, merge the unit branch into the repository's main branch. A conflicted merge is yours to resolve: keep both sides' behaviour (the brief's known sibling overlaps say where the sides meet), then run the project's full gate — the whole-tree suite — on the resolved tree; the resolution stands only when it is green. Report as blocked ({landed:false, blocked_on:"conflict", detail: what contradicts what}) only a genuine semantic contradiction — the same knob given different semantics, intent the sides cannot both keep — or a resolution whose gate stays red; abort the merge before reporting. Push main only if push_policy is "push-on-land"; under "hold-for-user" the merge stays local and detail says so. Reply with {landed, detail, merge_commit} — landed true only if the merge commit exists on main (verify with git log). Reply with the JSON last.`
    : `Publish the finished unit as a blocked PR. First read ${annexeConfigPath} for push_policy: under "hold-for-user", do NOT push — reply {landed:false, blocked_on:"push_policy hold-for-user", detail:...} and stop. Otherwise: write ${unitDir}/pr.md whole from ${unitDir}/report.md (What changed / Why / What must be true / Known gaps), push the branch (git push -u origin <branch>), open the PR via the forge API — POST /api/v1/repos/<owner>/<repo>/pulls, title "WIP: <unit title>", body from pr.md, head the branch, base ${integrationBranch} (the unit's integration lane — the structural branch for lane units, whose promotion PR carries them to main later; the parent branch for a stacked unit whose work builds on a sibling's unmerged branch; or main itself for a Lane: main unit); tokens under ~/.config/forgejo/ (instances.toml maps the origin host to url and token). Never push refs/for/* (AGit is forbidden — no tracked branch, no multiline body); on auth failure reply {landed:false, blocked_on:"forge auth", detail:...}. Confirm by API read that the PR exists with a non-empty body; landed is true only on that confirmation. Reply with {landed, detail, pr_number, pr_url} — the JSON last.`,
  { schema: landingSchema, cwd: worktree, label: "land", phase: "land", timeoutMs: 1800000 }
);

// The pipeline never writes the ledger: the orchestrator records the
// obligation's evidence from this returned envelope (references/ledger.md),
// and files issues_findings — sub-severe findings accepted by default at the
// land — to the annexe issue log.
return { outcome: landing && landing.landed ? "landed" : "landing-blocked", sketch, buildReport, verdict, fixReport, autonomous_rounds: autonomousRounds, issues_findings: issuesFindings, landing };
