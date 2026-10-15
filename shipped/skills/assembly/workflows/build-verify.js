// build-verify — the assembly skill's standard unit pipeline:
// admission-checked restatement (hold), build, cross-family semantic
// verification, autonomous fix rounds that converge severe findings before
// the orchestrator holds, per-finding triage of the residue (hold), then a
// landing stage that reports a derived result. Verification is a stage,
// not a decision.

export const meta = {
  description:
    "Build one unit from its brief: admission-checked restatement (hold), build, cross-family verification, fix rounds, per-finding triage of the residue (hold), and a landing stage reporting a derived result.",
  argumentSchema: {
    "type": "object",
    "properties": {
      "briefPath": {
        "type": "string",
        "description": "absolute path to the unit's brief"
      },
      "worktree": {
        "type": "string",
        "description": "absolute path to the unit's worktree"
      },
      "unitDir": {
        "type": "string",
        "description": "absolute path to the unit's record directory"
      },
      "mergePolicy": {
        "type": "string",
        "enum": [
          "direct-merge",
          "pr"
        ],
        "description": "how the unit lands"
      },
      "integrationBranch": {
        "type": "string",
        "description": "the branch the unit's PR targets; required when mergePolicy is \"pr\""
      },
      "annexeConfigPath": {
        "type": "string",
        "description": "absolute path to the annexe's config.toml (push_policy)"
      },
      "commissionPath": {
        "type": "string",
        "description": "absolute path to the commission (the annexe README)"
      },
      "spinePath": {
        "type": "string",
        "description": "absolute path to build/spine.md"
      },
      "verificationPath": {
        "type": "string",
        "description": "absolute path to VERIFICATION.md"
      }
    },
    "required": [
      "briefPath",
      "worktree",
      "commissionPath",
      "spinePath",
      "verificationPath",
      "unitDir",
      "mergePolicy",
      "annexeConfigPath"
    ]
  },
};

export const defaults = {
  // Builders ride the flagship GPT tier at low effort: the brief plus the
  // named exemplar have already decided the path, so the tier carries the
  // capability and effort stays the cost lever. The builder is one held
  // worker — restatement, build, fix rounds and a warm landing are turns of
  // a single session — and a held worker's model and effort are fixed when
  // it opens, so fix rounds run at the build pin: the context the worker
  // carries into a fix round substitutes for the search a higher effort
  // would buy, and escalation is what a park and a fresh re-brief are for.
  // Claude legs are the cross-family verifiers, deliberately below the top
  // tier (the top tier appears nowhere in assembly pipelines), pinned per
  // call; the verifier is held too, so every re-verification returns to the
  // reader that derived the claims. Pinned because per-model effort
  // defaults are uneven.
  model: "gpt-6.1-sol",
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
// at each hold the pipeline posts a "decision.needed" event carrying the
// hold's exact content and pauses on the run's event queue until the
// answering "decision.answer" event arrives — posted from outside the script by a
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

// ------------------------------------------------------- dispatch record
// Every dispatch records on the run journal whether it continued a held
// worker or re-briefed a fresh one, and why. The choice is this script's,
// made per stage: a later turn that profits from a worker's accumulated
// context — a fix round after its own build, a re-verification after its
// own verdict — continues the held worker; a stage whose value is
// independence, or whose envelope (model, effort, grants, placement) must
// differ from the worker's, re-briefs. `staleness()` is a journalled
// reading, so a decision taken on it replays identically.
const dispatching = (label, mode, reason) => log(`[dispatch] ${label}: ${mode} — ${reason}`);

// A held worker's idle gap against its route's threshold, phrased for the
// dispatch record: the recorded fact a warm-continue-or-re-brief decision
// rests on, never a guess.
const idleReading = async (worker) => {
  const reading = await staleness(worker);
  const phrase = reading.stale === true
    ? `idle ${reading.idleMs}ms, past the ${reading.thresholdMs}ms threshold of route ${reading.route}`
    : reading.stale === false
      ? `idle ${reading.idleMs}ms, within the ${reading.thresholdMs}ms threshold of route ${reading.route}`
      : `idle ${reading.idleMs}ms, no staleness threshold declared on route ${reading.route ?? "unknown"}`;
  return { ...reading, phrase };
};

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

// The builder is one held worker from restatement onward: the reading it
// does to restate the brief — the exemplar, the commission slice, the code
// around the extension point — is the context the build turn starts from,
// and a corrected restatement is a refinement of its own proposal. Its
// model, effort, placement and grants are fixed here; every later turn
// keeps them.
const builder = await session({ cwd: worktree, label: "builder" });
let sketch = null;
let holdDecision = null;
// Hold-decision notes — restate and triage alike — carried forward to every
// later verification: a ruling may supersede write-once brief text.
const rulings = [];
for (let attempt = 0; attempt < 3; attempt++) {
  dispatching(
    `restate:${attempt}`,
    attempt === 0 ? "re-briefed" : "continued",
    attempt === 0
      ? "the builder's held session opens on the brief; the build and its fix rounds continue this worker"
      : "the orchestrator corrected this worker's own restatement — it refines what it proposed rather than re-deriving the brief"
  );
  sketch = await send(builder, restatePrompt(holdDecision && holdDecision.note), {
    schema: restatementSchema,
    label: `restate:${attempt}`,
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

// The accepted sketch is handed back verbatim so the builder builds what
// was accepted — the orchestrator may have corrected it — not what it
// happens to recall proposing.
dispatching("build", "continued", "the build starts from the reading this worker did to restate: the exemplar, the commission slice and the code it extends are already in its context");
const buildReport = await send(
  builder,
  `Build the unit this accepted restatement describes — your own restatement, as the orchestrator accepted it over the brief at ${briefPath}; persist trust in it:

<accepted-restatement>
${JSON.stringify(sketch)}
</accepted-restatement>

Work only in this worktree (${worktree}); commit as you go with plain descriptive messages. Publication is the landing stage's, never yours: never push, never open PRs, never merge this unit's work outward — but merging ${mergePolicy === "pr" ? integrationBranch : "the repository's main branch"} into your own branch is ordinary maintenance, permitted when the unit's work needs it. Extend the named spine exemplar's pattern; its imitation boundary and envelope (its entry in ${spinePath}) are hard constraints — incidental wiring the extension requires (registration, imports, config, tests beside the code) is part of extending; modifying non-spine files beyond that wiring is not yours, report it as blocked instead. Verify your work within the unit's scope as the brief's proof expectation describes, then run the project's check gate and the tests covering the surfaces you touched, on the tree your build finishes on, and get them green — fix what they find and re-run as many times as that takes. The full test-suite gate is not yours: it runs later, batched over landed units. Every new test must discriminate — test something genuine that a broken product would fail, with a real failure the assertion can register, never a check the change's mere presence satisfies: write each test of behaviour this unit builds so it would fail without your change, and never let a fixture, seed, or mock itself supply the state the assertion checks; a test of behaviour that predates the unit proves its teeth by mutation instead — break the behaviour deliberately in a scratch copy, watch the test fail, restore. Demonstrating the failure is the preferred evidence because it is usually cheap; where the demonstration itself would need machinery out of proportion to the change — a separately triggered build estate, a reconstructed historical tree, a live external dependency, a bespoke process harness — do not build that machinery: state in your report why demonstration is disproportionate — covering both routes, the scratch-tree fail-before run and the mutation fallback (break the behaviour deliberately in a scratch copy, watch the test fail); mutation is usually cheap, so most would-be declarations fall to it — and what instead shows the test discriminates (the real state it runs over, the behaviour's own output its assertion reads, the specific failure a broken product would produce); its disposition is the orchestrator's. Where you cannot meet the brief's proof expectation honestly at its named rung — the surface never shows the state, or the product cannot produce it — do not manufacture the state: report the impossibility as a failure finding (what the rung cannot show, and the strongest honest proof you produced instead); its disposition is the orchestrator's. If you cannot get it green, say so in your report as a failure finding — what failed, what you tried — never as a quietly recorded red result. Later stages audit your final recorded run rather than repeat it.

Write ${unitDir}/report.md: what was built at one level above the diff; what was tried and abandoned, and why; what fought back; how you verified and what you did NOT exercise; the check gate's run and the touched-surface test runs — the exact commands, the tree sha they ran on, and the results; your overall confidence (high/medium/low) with a one-line why. Reply with a two-sentence summary and the head commit hash.`,
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

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — every JSON value in your reply is checked against the schema and the LAST valid one is read, so use exactly one fence and put nothing schema-shaped after it. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show a sample answer; the only JSON value in your reply is your actual answer.`;

let fixReport = null;
const autonomousRounds = [];

// The verifier is a held worker of the opposite family: opened blind to the
// builder's session, then continued for every re-verification, so the
// claims it derived before reading the report are what it re-examines.
const verifierEnvelope = { model: "claude-opus-5-5", effort: "medium", cwd: worktree, label: "verifier" };
let verifier = await session(verifierEnvelope);
dispatching("verify", "re-briefed", "cross-family verification is independent by design: a fresh reader of the opposite family, sharing nothing with the builder's session; its re-verifications continue this worker");

// A verifier turn that exhausts the findings schema returns null: the reader
// replied, but nothing it said validated as a findings report. That is a
// failed verification, never a clean one and never the operator's to
// untangle. The recovery is one fresh reader of the same envelope — a held
// worker cannot be re-shaped, and a second attempt by the reader that
// drifted repeats the drift — and a second exhaustion ends the run with an
// explicit outcome. The exhausted turns stay in the run record under their
// labels; the journal line names them so the cause can be read before any
// re-dispatch.
const verifyTurn = async (label, prompt) => {
  let result = await send(verifier, prompt, { schema: findingsSchema, label });
  if (result !== null) return result;
  log(`[verify] ${label}: the verifier replied, but nothing it said validated against the findings schema; its turns are in the run record under label ${label}. Re-briefing one fresh reader of the same envelope.`);
  await close(verifier);
  verifier = await session(verifierEnvelope);
  dispatching(label, "re-briefed", "the previous reader exhausted the findings schema; a fresh reader of the same envelope replaces it, because a held worker cannot be re-shaped and a second attempt by the reader that drifted repeats the drift");
  result = await send(verifier, prompt, { schema: findingsSchema, label: `${label}:retry` });
  if (result !== null) return result;
  log(`[verify] ${label}: the replacement reader also exhausted the findings schema; this unit's verification has failed.`);
  return null;
};
// The explicit failed outcome: nothing landed, no verdict exists, and the
// envelope says why. The lead treats it as a failed unit to re-dispatch
// after reading the exhausted turns, never as a clean result.
const verificationFailed = (label) => ({
  outcome: "verification-failed",
  reason: `the verifier produced no schema-valid findings report at stage ${label} across two readers; their replies are in the run record under labels ${label} and ${label}:retry`,
  sketch, buildReport, verdict: null, fixReport, autonomous_rounds: autonomousRounds, landing: null,
});

let verdict = await verifyTurn("verify", verifyPrompt(null, null));
if (verdict === null) return verificationFailed("verify");
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

Reply with JSON: summary (what you fixed, in two or three sentences), head_commit (the new head), blocked (the findings deliberately left unfixed, each by its 0-based index in the verdict, with why; empty array when none). Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — every JSON value in your reply is checked against the schema and the LAST valid one is read, so use exactly one fence and put nothing schema-shaped after it. A reply that is the bare JSON value alone also works.`;

let rounds = 0;
let handover = null;

while (severesIn(verdict).length) {
  if (autonomousRounds.length >= autonomousRoundLimit || rounds >= totalRoundLimit) {
    handover = "autonomous round limit reached with severe findings open";
    break;
  }
  const builderIdle = await idleReading(builder);
  dispatching(`fix:${rounds}`, "continued", `a fix round after its own build: the worker holds what it built and why, so the findings land on context rather than on a re-read of the worktree (${builderIdle.phrase}; continued regardless — the context is worth more than the cache)`);
  const autoFix = await send(builder, autoFixPrompt(rounds), {
    schema: autoFixSchema, label: `fix:${rounds}`, timeoutMs: 7200000,
  });
  if (!autoFix) throw new Error("autonomous fix round returned nothing usable — re-run, never read as applied");
  fixReport = autoFix.summary;
  const verifierIdle = await idleReading(verifier);
  dispatching(`verify:refix:${rounds}`, "continued", `a re-verification after its own verdict: the reader re-examines the claims it derived and the fix delta against what it already saw (${verifierIdle.phrase}; continued regardless)`);
  verdict = await verifyTurn(`verify:refix:${rounds}`, verifyPrompt(rounds, "autonomous"));
  if (verdict === null) return verificationFailed(`verify:refix:${rounds}`);
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
  dispatching(`fix:${rounds}`, "continued", `a directed fix round after its own build: the dispositions name findings against work this worker holds in context (${builderIdle.phrase}; continued regardless — the context is worth more than the cache)`);
  fixReport = await send(
    builder,
    `The verifier raised findings against your unit (${unitDir}/${rounds === 0 ? "verdict.md" : `verdict-refix-${rounds - 1}.md`}). The orchestrator's dispositions, by finding index: ${JSON.stringify(fixList)} — with this note: "${triage.note || ""}".${rulings.length ? `\n\nThe orchestrator's rulings below hold the brief's standing, and a conflicting ruling supersedes it — delivering what a ruling withheld is a defect, not a fix. Where two conflict, the later governs.\n<orchestrator-rulings>\n${rulings.map((r, i) => `${i + 1}. ${r}`).join("\n")}\n</orchestrator-rulings>\n` : ""} Your brief's standing constraints remain in force: work only in ${worktree}; never push, open PRs, or merge the unit's work outward — publication is the landing stage's, though merging the integration lane into the unit branch stays permitted where the work needs it; the spine exemplar's imitation boundary still binds. Apply exactly the dispositions marked fix — no more (an accepted-with-note finding is not yours to fix). A disposition you cannot apply honestly as written — the demanded proof or change cannot be produced without manufacturing state or asserting something untrue — stays unapplied: report it as blocked in your reply and in ${unitDir}/report.md, with why. A duplication finding whose verdict routes it to filing (adoption needs a cross-unit extraction) is not yours to perform either — leave it as it stands rather than growing the unit's diff. Commit as you go. Verify each fix with the checks covering the surfaces it touched — no project gate re-runs per fix round; the lane gate covers the composed result. Then make the whole of ${unitDir}/report.md true about the current tree — re-derive what changed from git log over the unit's commits and correct every section the fixes dated (built, tried-and-abandoned, verification, confidence, any commit inventory), not just the sections a finding quoted. Reply with a summary and the new head commit.`,
    { label: `fix:${rounds}`, timeoutMs: 7200000 }
  );
  const verifierIdle = await idleReading(verifier);
  dispatching(`verify:refix:${rounds}`, "continued", `a re-verification after its own verdict, over the delta the orchestrator's dispositions authorised (${verifierIdle.phrase}; continued regardless)`);
  verdict = await verifyTurn(`verify:refix:${rounds}`, verifyPrompt(rounds, "directed"));
  if (verdict === null) return verificationFailed(`verify:refix:${rounds}`);
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
    already_integrated: { type: "boolean" },
    pr_number: { type: "integer" },
    pr_url: { type: "string" },
    blocked_on: { type: "string" },
  },
};

const landPrompt =
  mergePolicy === "direct-merge"
    ? `Land the finished unit. First read ${annexeConfigPath} for push_policy. From ${worktree}, merge the unit branch into the repository's main branch. A conflicted merge is yours to resolve: keep both sides' behaviour (the brief's known sibling overlaps say where the sides meet), then run the project's full gate — the whole-tree suite — on the resolved tree; the resolution stands only when it is green. Report as blocked ({landed:false, blocked_on:"conflict", detail: what contradicts what}) only a genuine semantic contradiction — the same knob given different semantics, intent the sides cannot both keep — or a resolution whose gate stays red; abort the merge before reporting. Push main only if push_policy is "push-on-land"; under "hold-for-user" the merge stays local and detail says so. A merge that reports the branch already contained in main ("Already up to date") is an already-integrated unit, not a blocked landing: nothing changed, so there is nothing to abort — run the full gate on main as usual and reply {landed:true, already_integrated:true, merge_commit: the main commit that contains the branch head, detail: "already integrated; gate green" or "already integrated; gate red: <what failed>"} — a red gate on a main this unit did not change is lane state for the orchestrator, never a blocked landing. Reply with {landed, detail, merge_commit} — landed true only if the merge commit exists on main (verify with git log) or main already contains the branch head. landed:false always carries blocked_on naming what blocked the landing. Reply with the JSON last.`
    : `Publish the finished unit as a blocked PR. First read ${annexeConfigPath} for push_policy: under "hold-for-user", do NOT push — reply {landed:false, blocked_on:"push_policy hold-for-user", detail:...} and stop. Otherwise: write ${unitDir}/pr.md whole from ${unitDir}/report.md (What changed / Why / What must be true / Known gaps), push the branch (git push -u origin <branch>), open the PR via the forge API — POST /api/v1/repos/<owner>/<repo>/pulls, title "WIP: <unit title>", body from pr.md, head the branch, base ${integrationBranch} (the unit's integration lane — the structural branch for lane units, whose promotion PR carries them to main later; the parent branch for a stacked unit whose work builds on a sibling's unmerged branch; or main itself for a Lane: main unit); tokens under ~/.config/forgejo/ (instances.toml maps the origin host to url and token). Never push refs/for/* (AGit is forbidden — no tracked branch, no multiline body); on auth failure reply {landed:false, blocked_on:"forge auth", detail:...}. Confirm by API read that the PR exists with a non-empty body; landed is true only on that confirmation. Before pushing, check whether ${integrationBranch} already contains the branch head (git fetch, then git merge-base --is-ancestor <branch> origin/${integrationBranch}): if it does, there is nothing to publish — reply {landed:true, already_integrated:true, detail: the containing commit} with no PR opened and no pr_number. landed:false always carries blocked_on naming what blocked the landing. Reply with {landed, detail, pr_number, pr_url} — the JSON last.`;

// The landing continues the builder while its session is warm — it knows
// the unit it is publishing — and re-briefs a fresh worker once the triage
// hold has outlived the route's cache: the landing is mechanical, and
// report.md carries what a fresh worker needs, so a cold re-read of the
// whole build buys nothing. The reading decides; the reason records it.
const builderIdle = await idleReading(builder);
let landing;
if (builderIdle.stale === false) {
  dispatching("land", "continued", `the builder publishes its own unit while its session is warm (${builderIdle.phrase})`);
  landing = await send(builder, landPrompt, { schema: landingSchema, label: "land", timeoutMs: 1800000 });
} else {
  dispatching("land", "re-briefed", `the landing is mechanical and ${unitDir}/report.md carries what it needs; ${builderIdle.stale === true ? "the builder's session is cold" : "the route declares no staleness threshold, so warmth is unknown"} (${builderIdle.phrase})`);
  landing = await agent(landPrompt, { schema: landingSchema, cwd: worktree, label: "land", timeoutMs: 1800000 });
}
// A landing turn that exhausts the landing schema returns null: the worker
// replied, but nothing it said validated. One fresh worker is re-briefed
// (report.md carries what a landing needs); a second exhaustion is a
// blocked landing whose blocked_on names the cause, so the envelope keeps
// the shape the lead reads and the journal says where the replies are.
if (landing === null) {
  log("[land] the landing worker replied, but nothing it said validated against the landing schema; its turns are in the run record under label land. Re-briefing one fresh worker.");
  dispatching("land:retry", "re-briefed", "the previous landing worker exhausted the landing schema; a fresh worker reads report.md and lands from it");
  landing = await agent(landPrompt, { schema: landingSchema, cwd: worktree, label: "land:retry", timeoutMs: 1800000 });
  if (landing === null) {
    log("[land] the replacement landing worker also exhausted the landing schema; the landing is blocked.");
    landing = {
      landed: false,
      blocked_on: "landing reply never validated against the landing schema",
      detail: "two landing workers replied without a schema-valid landing report; their turns are in the run record under labels land and land:retry — read them before re-dispatching",
    };
  }
}
// Closed on the success path so the journal records intent; the early
// returns above are run-ending, and a run ending by any path closes every
// worker it still holds.
await close(builder);
await close(verifier);

// The pipeline never writes the ledger: the orchestrator records the
// obligation's evidence from this returned envelope (references/ledger.md),
// and files issues_findings — sub-severe findings accepted by default at the
// land — to the annexe issue log.
// A reply that claims a no-op and a publication at once is no verdict: the
// envelope keeps the shape the lead reads, with blocked_on naming the
// contradiction, rather than classifying on one half of it.
if (landing && landing.landed && landing.already_integrated === true
    && (landing.pr_number !== undefined || (landing.pr_url && landing.pr_url.trim()))) {
  log(`[land] the landing reply contradicts itself: already_integrated with a PR (${landing.pr_number ?? landing.pr_url}); not read as a landing.`);
  landing = {
    landed: false,
    blocked_on: "landing reply contradicts itself: already_integrated together with a PR",
    detail: `the worker replied ${JSON.stringify(landing)}; read its turns in the run record before re-dispatching`,
  };
}
// Three landing outcomes, derived from the returned envelope: "landed" (a
// merge commit or a confirmed PR), "already-integrated" (the lane already
// held the branch head; landed stays true so the lead reads it as done), and
// "landing-blocked" (landed false, with blocked_on naming the cause).
const landingOutcome = !landing || !landing.landed
  ? "landing-blocked"
  : landing.already_integrated === true ? "already-integrated" : "landed";
return { outcome: landingOutcome, sketch, buildReport, verdict, fixReport, autonomous_rounds: autonomousRounds, issues_findings: issuesFindings, landing };
