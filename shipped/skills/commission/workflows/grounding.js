// grounding — blind cross-family codebase grounding for the shedding run's
// beat 2: per candidate feature, one model family investigates the code repo
// to judge whether it is actually BUILT (blind to what anyone expects or
// remembers), then — only for the candidates judged built, the shed direction
// that deletes intent — the OTHER family independently verifies the claim
// before it is allowed to shed. Returns a cut/keep list; a feature sheds only
// on a positive find AND a cross-family confirm.

// Worker policy — declared once per family; every agent() call spreads its
// family's pins, and call sites override only where a stage's residue earns
// more, with a comment naming it. The default posture is the find stage:
// retrieval-shaped code-reading at the resting level. Pin model and effort
// together on every call: an unpinned call has no model to ride.
const FAMILY = {
  gpt: { model: "gpt-5.6-sol", effort: "medium" },
  claude: { model: "claude-sonnet-5", effort: "medium" },
};

// This is shedding's beat 2 (see references/shedding-workflow.md).
// Workers must read the real code: the repo path arrives as an arg and every
// worker is placed there (cwd). Keep the expected result blind: describe
// the capability in observable terms, never as a claim to confirm.
//
// A feature sheds only when the finder says "built" AND the
// opposite family independently confirms it. Anything else — not-built, partial, unknown, a
// finder that produced nothing, a verifier that could not confirm — KEEPS the feature. The
// cross-family verify runs only for the "built" set.
//
// Do not relax that two-positive rule: nothing downstream re-checks it. A false "built"
// sheds a feature that was never built — compressed to a sentence, detail demoted to
// shed/ — and the preservation passes read the relocation as accounted-for, not as a
// loss. A false "not-built" only leaves a feature uncompressed, and self-corrects on the
// next run.
//
// INPUT (pass as the run's args):
//   {
//     "repo": "/abs/path/to/code/repo",          // workers read this; REQUIRED
//     "candidates": [                             // the beat-1 sweep list
//       {
//         "id":      "c1",                         // stable handle for this candidate
//         "feature": "what a user/caller can do, or what the code would contain, in OBSERVABLE terms — NOT 'the README says ...'",
//         "where":   "where it lives in the README (optional, for the main session's own bookkeeping)"
//       }
//     ]
//   }
//
// OUTPUT (the run's result): { repo, cut_keep_list[] }. Each entry carries the verdict
// (shed|keep), the finder's build judgement and evidence, and — for the built set — the
// cross-family verifier's confirmation and evidence. The main session sheds the "shed"
// entries, keeps and reports the rest, and surfaces any finder/verifier disagreement to the
// human rather than resolving it silently. The cut_keep_list is also the
// `deliberate_changes.cut_keep_list` input the preservation workflow expects.
//
// An entry may also carry `verifier_out_of_scope_concerns` (null when the verifier had
// none): the verifier's objection to the QUESTION rather than its answer — the candidate
// describes the capability wrongly, or a true/false verdict would mislead either way.
// Read these BEFORE trusting the verdicts beside them, and take them to the human: a
// mis-stated candidate can earn a clean "confirmed built" and shed the wrong thing.

const cfg = args || {};
const repo = (cfg.repo || "").trim();
const candidates = Array.isArray(cfg.candidates) ? cfg.candidates : [];
if (!repo) throw new Error("grounding needs a repo path for the workers to investigate");
if (candidates.length === 0) {
  // An empty sweep is a valid answer — nothing to ground, nothing to shed.
  return { repo, cut_keep_list: [] };
}

// Load-balance the finders 50/50 so neither family dominates the fact pool; the verifier is
// always the OTHER family, so a "built" call is never confirmed only by its own kind.
const finderFamily = (i) => (i % 2 === 0 ? "gpt" : "claude");
const opposite = (family) => (family === "gpt" ? "claude" : "gpt");

// Verify is an adversarial re-derivation whose "yes" deletes intent from the
// contract — the named residue that earns the step up from the declared
// family pins.
const VERIFY_OPTS = {
  gpt: { effort: "high" },
  claude: { model: "claude-opus-5", effort: "high" },
};

// Keep reply schemas an object with additionalProperties:false and every
// property required, with explicit "empty when none" conventions — a worker
// compelled to emit every declared property must have an honest blank
// available, and a null result can then only mean worker collapse.
const findingSchema = {
  type: "object",
  additionalProperties: false,
  required: ["built", "evidence", "files", "observed_vs_inferred"],
  properties: {
    built: { type: "string", enum: ["built", "partial", "not-built", "unknown"] },
    // What was found, with file paths and quoted code — never a bare verdict.
    evidence: { type: "string" },
    files: { type: "array", items: { type: "string" } },
    // Grounding rule made structural: keep what the code shows apart from what you infer.
    observed_vs_inferred: { type: "string" },
  },
};

const verifySchema = {
  type: "object",
  additionalProperties: false,
  required: ["confirmed_built", "reason", "evidence", "out_of_scope_concerns"],
  properties: {
    // True ONLY if the verifier independently finds it genuinely and completely implemented.
    confirmed_built: { type: "boolean" },
    reason: { type: "string" },
    evidence: { type: "string" },
    // Somewhere to say "your question is the wrong one". A verifier that finds the
    // candidate's stated capability mis-described — so that confirming or denying it
    // both mislead — has no way to report that through confirmed_built, and without
    // this field the objection gets folded into hedging about the assigned question
    // and lost. Required with an explicit empty-string convention rather than
    // optional: a schema-bound worker is compelled to emit every declared
    // property, so an optional field would invite invented content instead of
    // an honest blank.
    out_of_scope_concerns: { type: "string" },
  },
};

const findPrompt = (candidate) =>
  `You are investigating a codebase to determine whether a specific capability is actually
implemented. Judge ONLY the code you find — you are blind to what anyone expects, planned, or
remembers, and that is deliberate. Do not assume it is built because it sounds finished.

The code repository to investigate:
${repo}

Read the code there. The capability to check, in observable terms:
${candidate.feature}

Decide which of these the code supports, and report it as \`built\`:
- "built": the capability is genuinely and completely implemented in the code.
- "partial": started or stubbed, but not complete enough to stand as its own documentation.
- "not-built": no implementation found.
- "unknown": you could not determine it from the code available.

Grounding rules — these make the finding usable rather than noise:
- Attach evidence to your verdict: name the files (\`files\`) and quote the code that decides
  it (\`evidence\`). A verdict with no evidence is not a finding.
- Quote the code; do not paraphrase what you think it does.
- "unknown" / "could not verify" is a valid, welcome answer. Do NOT round an uncertain read
  up to "built" — a wrong "built" is the expensive error here.
- Keep what you OBSERVE in the code separate from what you INFER (\`observed_vs_inferred\`).

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show a sample answer; the only JSON value in your reply is your actual answer.`;

const verifyPrompt = (candidate, finding) =>
  `You are independently verifying a claim that a codebase capability is FULLY implemented. A
prior investigation, by a different model, concluded it is built. Your job is not to agree —
it is to check for yourself and hunt for why that conclusion is WRONG: is it actually only
partial, stubbed, scaffolded, or absent once you read the code? Confirm it built only if you
independently find it so.

The code repository:
${repo}

The capability claimed built, in observable terms:
${candidate.feature}

What the prior investigation reported (treat as a claim to test, not as truth):
${JSON.stringify({ files: finding.files, evidence: finding.evidence }, null, 2)}

Read the code yourself. Set \`confirmed_built\` true ONLY if you independently find the
capability genuinely and completely implemented; set it false if it is partial, stubbed, or
absent, or if you cannot confirm it from the code. Attach your own evidence — quote the code
and name files — and give your \`reason\`. "Could not confirm" maps to false: when in doubt,
do not confirm, because confirming wrongly is what deletes a real feature from the contract.

If something is wrong with the QUESTION rather than the answer — the capability as described
does not match what the code is organised around, the feature is real but the description
names the wrong thing, or a true/false verdict would mislead whichever way it went — say so
in \`out_of_scope_concerns\`. That field is for what this task did not ask you about; use an
empty string when you have nothing, and never pad it.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — the FIRST fenced block in your reply is the one that is read, so use exactly one and put nothing schema-shaped in any other fence. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// One pipeline per candidate: find → (conditionally) cross-family verify. pipeline keeps a
// fast candidate moving to verification while a slow one is still being investigated, and
// items are independent — there is no cross-candidate barrier to wait on.
// Every dispatch records on the run journal whether it continued a held
// worker or re-briefed a fresh one, and why — the choice is this script's,
// made per stage (the shipped set's rule: continue where a later turn
// profits from a worker's accumulated context, re-brief where independence
// is the point or the envelope differs).
const dispatching = (label, mode, reason) => log(`[dispatch] ${label}: ${mode} — ${reason}`);

phase("ground");
const items = candidates.map((c, i) => ({ ...c, i }));
log(`grounding ${items.length} candidate(s) against ${repo}`);

const results = await pipeline(
  items,
  // Stage 1 — FIND. Load-balanced across both families; each candidate found by one.
  (item) => {
    const finder = finderFamily(item.i);
    dispatching(`find:${finder}:${item.i}`, "re-briefed", "each candidate is found blind by one family; a finder sharing another candidate's context would read the code with expectations");
    return agent(findPrompt(item), {
      ...FAMILY[finder],
      cwd: repo,
      schema: findingSchema,
      label: `find:${finder}:${item.i}`,
    }).then((finding) => ({ item, finder, finding }));
  },
  // Stage 2 — VERIFY, but only the shed direction. Not-built/partial/unknown keep without
  // spending a cross-family check; "built" is confirmed by the OTHER family before it sheds.
  (prev, item) => {
    if (!prev || !prev.finding) {
      // Finder collapsed after retries — keep conservatively; never shed on no evidence.
      return {
        item,
        finder: prev ? prev.finder : null,
        finding: null,
        verifier: null,
        verification: null,
        verdict: "keep",
        reason: "finder produced no result; kept (never shed without evidence)",
      };
    }
    const finding = prev.finding;
    if (finding.built !== "built") {
      // The safe verdicts: keep the feature fully elaborated, no cross-family spend needed.
      return {
        ...prev,
        verifier: null,
        verification: null,
        verdict: "keep",
        reason: `finder judged "${finding.built}"`,
      };
    }
    const verifier = opposite(prev.finder);
    dispatching(`verify:${verifier}:${item.i}`, "re-briefed", "the verifier is the opposite family by design and reads the code itself: a finder continuing into its own verification would confirm its own call");
    return agent(verifyPrompt(item, finding), {
      ...FAMILY[verifier],
      ...VERIFY_OPTS[verifier],
      cwd: repo,
      schema: verifySchema,
      label: `verify:${verifier}:${item.i}`,
    }).then((verification) => ({
      ...prev,
      verifier,
      verification,
      // Shed ONLY on a positive find AND a cross-family confirm. Anything else keeps.
      verdict: verification && verification.confirmed_built === true ? "shed" : "keep",
      reason:
        verification == null
          ? "cross-family verification inconclusive; kept (built unconfirmed)"
          : verification.confirmed_built
            ? "built (finder) and confirmed built (cross-family verifier)"
            : `finder said built, but cross-family verifier did not confirm: ${verification.reason}`,
    }));
  },
);

// pipeline() aligns its result array to the input items positionally, so a null
// slot — the whole pipeline stage failed for that candidate, rare since a
// schema agent inside a pipeline slot collapses to null rather than
// rejecting (note: an unknown option key is silently dropped, not
// rejected — pin options carefully) — is recovered by index. A cut/keep list
// exists to be complete: never let a candidate vanish from it. A failed one keeps,
// flagged, so the main session knows it went unevaluated rather than silently gone.
const cut_keep_list = results.map((r, i) => {
  if (!r) {
    const item = items[i];
    return {
      id: item.id,
      feature: item.feature,
      where: item.where || null,
      verdict: "keep",
      built: "unknown",
      finder: null,
      verifier: null,
      confirmed_built: null,
      finder_evidence: null,
      finder_files: null,
      verifier_evidence: null,
      verifier_out_of_scope_concerns: null,
      reason: "grounding pipeline failed for this candidate; kept (never shed unevaluated)",
    };
  }
  return {
    id: r.item.id,
    feature: r.item.feature,
    where: r.item.where || null,
    verdict: r.verdict, // "shed" | "keep"
    built: r.finding ? r.finding.built : "unknown",
    finder: r.finder || null,
    verifier: r.verifier || null,
    confirmed_built: r.verification ? r.verification.confirmed_built : null,
    finder_evidence: r.finding ? r.finding.evidence : null,
    finder_files: r.finding ? r.finding.files : null,
    verifier_evidence: r.verification ? r.verification.evidence : null,
    // A verifier's objection to the QUESTION, carried through to the main session:
    // it is read before the verdict beside it is trusted, so it must not be dropped
    // here just because the verdict itself was clean.
    verifier_out_of_scope_concerns:
      r.verification && (r.verification.out_of_scope_concerns || "").trim()
        ? r.verification.out_of_scope_concerns.trim()
        : null,
    reason: r.reason,
  };
});

const shedCount = cut_keep_list.filter((c) => c.verdict === "shed").length;
log(`${shedCount}/${cut_keep_list.length} candidate(s) cleared to shed (built + cross-family confirmed)`);
// Surfaced separately from the verdicts: an objection to the question is exactly
// what a clean-looking shed list would otherwise bury.
const flagged = cut_keep_list.filter((c) => c.verifier_out_of_scope_concerns);
if (flagged.length) {
  log(`${flagged.length} candidate(s) carry a verifier concern about the question itself — read these before acting on the verdicts: ${flagged.map((c) => c.id).join(", ")}`);
}

return { repo, cut_keep_list };
