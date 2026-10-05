// commitment-audit — pre-sign-off audit of a draft commitment register:
// cheap cross-family miners extract every want the human voiced from the
// conversation's own transcript files (with read receipts), a blind
// cross-family diff returns the wants no commitment covers (the exclusion
// diff the human signs over), and both families record the cheapest
// implementation that survives the whole register, judged per in-scope line
// (hollow verdicts, to disposition before signature). Recorded, fail-visible
// gates: a missing leg is reported as a gap, never absorbed into a
// clean-looking result.

// Worker policy — declared once per family; every agent() call spreads its
// family's pins, and stages override only where their residue earns more,
// with a comment naming it. The default posture is the mining stage:
// high-volume verbatim extraction where judgement is not the bottleneck, so
// the GPT side pins the volume tier and Claude the cheap tier, both at the
// floor — the brief decides the path. Pin model and effort together on every
// call: an unpinned call has no model to ride.
const FAMILY = {
  gpt: { model: "gpt-6-luna", effort: "low" },
  claude: { model: "claude-sonnet-5-5", effort: "low" },
};

// Run this before the sign-off read of any round that worked product intent
// (see SKILL.md, Sign-off — the trigger is the conversation's intent, not
// whether a line's bytes changed). Both families must be reachable — the
// point of
// mining and judging cross-family is that correlated blind spots in one
// family do not silently agree with themselves; the degraded single-family
// rendering is governed by references/ensemble-workflows.md (Hard
// requirement), never chosen silently.
//
// The miners read the conversation's transcript FILES from disk (locate them
// per SKILL.md, Session Transcripts) — never a summary of the conversation:
// a summary can only be checked against itself, and the want it dropped is
// exactly the want this audit exists to find.
//
// INPUT (pass as the run's args):
//   {
//     "transcripts": ["/abs/path/to/transcript-1.jsonl", ...],  // REQUIRED; every file of THIS conversation
//     "commitments": [                                          // REQUIRED; the WHOLE draft register, bare
//       { "id": "C1", "text": "the full commitment line, falsifier included" }
//     ],
//     "hollow_ids": ["C7", "C9"]   // optional: scope the hollow stage to these
//                                  // ids (lines created/changed this round plus
//                                  // any never audited). Omitted = every line.
//   }
//
// OUTPUT (the run's result):
//   { mined_wants[], mining_gaps[], exclusion_diff[], diff_concerns[], diff_gaps[], hollow_verdicts[] }.
//   - mined_wants: every want either family extracted, with its verbatim quote.
//   - mining_gaps: legs that failed, could not fully read their transcript, or
//     twice returned zero wants (a zero-want leg is retried once before the gap
//     is recorded — a commission conversation that voices nothing is rare
//     enough that an empty pool is presumed a mining failure until confirmed) —
//     while non-empty, the exclusion diff is NOT the two-family union it claims;
//     re-run the failed legs, confirm a genuinely wantless transcript with the
//     user, or treat the audit as degraded.
//   - exclusion_diff: wants neither matched by the diff stage to any commitment —
//     every entry a want a human voiced, carrying its verbatim quote — each
//     settled with the human (a new line, a hold, a non-binding tail
//     mention or IDEAS.md entry, or a deliberate exclusion carried onto the
//     sign-off read); the union of both families' unmatched sets, deduplicated
//     by the adjudicating session, not here.
//   - diff_concerns: per-family remarks about the matching question itself (a
//     want that is not capability-shaped, lists mis-framed) — read them, but
//     they are never wants and never enter the exclusion diff.
//   - diff_gaps: families whose differ collapsed, plus the case where no mined
//     wants reached the diff at all — same rule as mining_gaps.
//   - hollow_verdicts: per in-scope commitment, BOTH families' cheapest
//     implementations that survive the WHOLE register; a line either family
//     judged hollow is dispositioned before signature (sharpened, recorded as
//     covered by siblings, or accepted as a residual — SKILL.md, Sign-off), a
//     verdict's covered_by_siblings names register lines that already defeat
//     the cheap reading, and a line with no
//     verdict from a family is degraded (one family) or unaudited (both) —
//     a check still owed, never a pass.
// Read out_of_scope_concerns fields before trusting the verdicts beside them.

const cfg = args || {};
const transcripts = Array.isArray(cfg.transcripts) ? cfg.transcripts.filter(Boolean) : [];
const commitments = Array.isArray(cfg.commitments) ? cfg.commitments.filter(Boolean) : [];
const hollowIds = Array.isArray(cfg.hollow_ids) ? cfg.hollow_ids.filter(Boolean) : null;
if (transcripts.length === 0) throw new Error("commitment-audit needs at least one transcript path");
if (commitments.length === 0) throw new Error("commitment-audit needs the draft commitment lines");
const hollowScope = hollowIds
  ? commitments.filter((c) => hollowIds.includes(c.id))
  : commitments;

// Judgement stages step up from the mining floor: the diff is a careful read
// of two lists (mid effort, volume tier still suffices); the hollow test is
// adversarial judgement whose miss signs a vacuous contract — the named
// residue that earns the flagship/opus step.
const DIFF_OPTS = {
  gpt: { model: "gpt-6.1-sol", effort: "medium" },
  claude: { model: "claude-sonnet-5-5", effort: "medium" },
};
const HOLLOW_OPTS = {
  gpt: { model: "gpt-6.1-sol", effort: "medium" },
  claude: { model: "claude-opus-5-5", effort: "medium" },
};

// Keep reply schemas an object with additionalProperties:false and every
// declared property required, each with an explicit empty-value convention —
// a schema-bound worker is compelled to emit each one, so an honest blank
// must exist.
const mineSchema = {
  type: "object",
  additionalProperties: false,
  required: ["read_complete", "read_note", "wants"],
  properties: {
    // The read receipt: true ONLY when the whole file was actually read. A
    // transcript that could not be opened or was too large to finish is a
    // failed check, not an empty result.
    read_complete: { type: "boolean" },
    // What was read and what was not (byte ranges, truncation, errors);
    // empty string when the read was complete and unremarkable.
    read_note: { type: "string" },
    wants: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["want", "quote"],
        properties: {
          // One voiced want, phrased as the capability it asks for, in
          // observable terms.
          want: { type: "string" },
          // The human's own words, verbatim from the transcript, that voice it.
          quote: { type: "string" },
        },
      },
    },
  },
};

const diffSchema = {
  type: "object",
  additionalProperties: false,
  required: ["unmatched", "out_of_scope_concerns"],
  properties: {
    unmatched: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["want", "quote", "nearest_commitment", "reason"],
        properties: {
          want: { type: "string" },
          quote: { type: "string" },
          // The closest commitment ID considered, or an empty string when none
          // came close — evidence the match was actually hunted for.
          nearest_commitment: { type: "string" },
          reason: { type: "string" },
        },
      },
    },
    // Somewhere to say the question is wrong (a list mis-framed, a want that is
    // not capability-shaped); empty string when none — never padded.
    out_of_scope_concerns: { type: "string" },
  },
};

const hollowSchema = {
  type: "object",
  additionalProperties: false,
  required: ["cheapest_implementation", "hollow", "covered_by_siblings", "reason", "out_of_scope_concerns"],
  properties: {
    // The cheapest implementation that satisfies the whole register's words
    // exactly.
    cheapest_implementation: { type: "string" },
    // True when that implementation delivers the letter of the words while the
    // capability a reasonable reader would expect from the line under test is
    // absent.
    hollow: { type: "boolean" },
    // Comma-separated sibling commitment IDs whose words defeat the line's own
    // cheap reading; empty string when none apply.
    covered_by_siblings: { type: "string" },
    reason: { type: "string" },
    out_of_scope_concerns: { type: "string" },
  },
};

const minePrompt = (path) =>
  `You are auditing a planning conversation for capture completeness. Read the transcript file
at this path — it is a machine log of a conversation between a human and an assistant about
what a project should become:

${path}

First, the read receipt: set \`read_complete\` true only if you actually read the whole file.
If you could not open it, or could not finish it, say exactly what you did and did not read
in \`read_note\` and set \`read_complete\` false — a partial read reported honestly is useful;
a partial read presented as complete poisons the audit downstream.

Extract every want the HUMAN voiced about the finished product: capabilities asked for,
expectations stated, behaviours assumed aloud, reactions that imply a requirement ("it must
not lose the run", "I'd expect to search by phrase"). The human's words live in TWO places
in these logs: messages with a user role, and the human's answers to the assistant's
structured questions, where the log embeds them inside tool results as answer
values — often long free text. Mine both: a question-driven
conversation carries most of its decisions in the second place, so a transcript with few
user-role messages is not a transcript with few wants. Work from the human's words; the
assistant's proposals count only where the human took them up. A want the human voiced
COUNTS even when the assistant argued against it, deferred it, or the conversation moved
on — wants that were voiced and then dropped are precisely what this audit exists to
surface. The transcript may contain the assistant's draft contract text; its presence is
not evidence a want is covered — extraction is your only job, matching happens elsewhere.
Ignore conversation about process, tooling for this conversation itself, or pleasantries.

For each want: phrase it as the capability it asks for, in observable terms (\`want\`), and
attach the human's own words verbatim from the transcript (\`quote\`) — the quote is what
lets the finding be checked, so never paraphrase inside it. Prefer over-listing to
filtering: a duplicate is cheap, a dropped want is the failure this audit exists to catch.
An empty list is valid only for a completely-read transcript that voices none.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — every JSON value in your reply is checked against the schema and the LAST valid one is read, so use exactly one fence and put nothing schema-shaped after it. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

const diffPrompt = (wants, lines) =>
  `You are checking a project contract for coverage. Below are two lists and nothing else —
that is deliberate: judge only what is on the lists.

<voiced_wants>
${wants.map((w, i) => `${i + 1}. ${w.want}\n   quote: ${w.quote}`).join("\n")}
</voiced_wants>

<commitments>
${lines.map((c) => `${c.id}: ${c.text}`).join("\n")}
</commitments>

For each voiced want, hunt for a commitment that genuinely covers it — the capability the
want asks for would exist if that commitment's words were satisfied non-vacuously. A want
phrased as keeping something from happening ("X must never…", "keep X never happening") is
covered by a commitment whose words affirmatively rule the outcome out — a boundary
statement, an "absent when" / "violated when" limb — so hunt those clauses too, not only
capability grants, before concluding nothing covers it. Return
ONLY the wants no commitment covers (\`unmatched\`), each with the closest commitment you
considered (\`nearest_commitment\`, empty string when nothing came close) and why it does
not cover the want (\`reason\`). Every \`unmatched\` entry is one of the numbered wants
above, carried with its \`quote\` verbatim — never a remark of your own about a want or
about the lists. Duplicate wants collapse to one entry. Do not pad the list:
a covered want is simply omitted, and an empty \`unmatched\` is a valid answer.

If something is wrong with the QUESTION rather than the answer — a want that is not
capability-shaped, a want that resists the match-or-exclude binary, lists that seem
mis-framed — say so in \`out_of_scope_concerns\`, which exists so such remarks never need
to ride the \`unmatched\` list; empty string when you have nothing.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — every JSON value in your reply is checked against the schema and the LAST valid one is read, so use exactly one fence and put nothing schema-shaped after it. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

const hollowPrompt = (line, register) =>
  `You are stress-testing one line of a project contract. The whole register is below,
because the builder who eventually satisfies the line reads the contract whole: a gap one
line's words leave may be closed by the words of another.

<register>
${register.map((c) => `${c.id}: ${c.text}`).join("\n")}
</register>

<line_under_test>
${line.id}: ${line.text}
</line_under_test>

Describe the CHEAPEST implementation that satisfies the words of the ENTIRE register at
once (\`cheapest_implementation\`) — the laziest thing a builder could ship while truthfully
claiming every line is met, including passing each line's "absent when" / "violated when"
observation. Then judge the line under test against that implementation: does it deliver
the letter of the register's words while the capability a reasonable reader would expect
from THIS line is absent (\`hollow\`: true), or do the words as written force the real
capability (\`hollow\`: false)? Give your \`reason\`. When the line's own words admit a cheap
reading that OTHER lines' words defeat, that is not hollow: set \`hollow\` false and name
those lines in \`covered_by_siblings\` (their IDs, comma-separated; empty string
otherwise). Err toward hollow when in
doubt — a false alarm costs one sharpening pass; a missed hollow line signs a contract a
trivial implementation can satisfy.

If something is wrong with the QUESTION rather than the answer — the line is not
capability-shaped, or hollow/not-hollow would mislead either way — say so in
\`out_of_scope_concerns\`; empty string when you have nothing.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — every JSON value in your reply is checked against the schema and the LAST valid one is read, so use exactly one fence and put nothing schema-shaped after it. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Stage 1 — MINE. Every transcript read independently by BOTH families, so a
// want one family's reading habitually drops is caught by the other; the union
// is deliberately over-inclusive. Failed or partial legs are RECORDED, not
// filtered away: a mined pool missing a leg is not the two-family union. A
// completed read that returns ZERO wants is treated the same way: a commission
// conversation that voices nothing is rare enough that an empty list is
// presumed a mining failure — the leg is retried once, and a second zero is
// recorded as a gap for the adjudicating session to confirm, never flowed
// silently into the diff as a vacuous clean pool.
// Every dispatch records on the run journal whether it continued a held
// worker or re-briefed a fresh one, and why — the choice is this script's,
// made per stage (the shipped set's rule: continue where a later turn
// profits from a worker's accumulated context, re-brief where independence
// is the point or the envelope differs).
const dispatching = (label, mode, reason) => log(`[dispatch] ${label}: ${mode} — ${reason}`);

phase("mine");
log(`mining ${transcripts.length} transcript file(s) with both families`);
const mineLegs = [];
for (const path of transcripts) {
  for (const family of ["gpt", "claude"]) {
    mineLegs.push({ path, family });
  }
}
const runMineLegs = (legs, retry) =>
  parallel(
    legs.map((leg) => () => {
      dispatching(`mine:${leg.family}`, "re-briefed", retry
        ? "a zero-want read is presumed a mining failure, so the retry is a fresh read of the transcript rather than a continuation that would re-affirm the empty result"
        : "each transcript is read independently by both families; a miner sharing another's context would inherit the drops the other family's read makes");
      return agent(minePrompt(leg.path), {
        ...FAMILY[leg.family],
        schema: mineSchema,
        label: `mine:${leg.family}`,
      }).then((r) => ({ ...leg, result: r }));
    }),
  );
const mined = await runMineLegs(mineLegs, false);
const mining_gaps = [];
const minedWants = [];
const zeroWantLegs = [];
mined.forEach((m, i) => {
  const leg = m || mineLegs[i];
  const r = m && m.result;
  if (!r) {
    mining_gaps.push({ path: leg.path, family: leg.family, gap: "worker collapsed; leg produced nothing" });
    return;
  }
  if (r.read_complete !== true) {
    mining_gaps.push({ path: leg.path, family: leg.family, gap: `incomplete read: ${r.read_note || "no detail given"}` });
  }
  if (r.wants.length === 0) {
    zeroWantLegs.push({ path: leg.path, family: leg.family });
    return;
  }
  for (const w of r.wants) minedWants.push({ ...w, family: leg.family });
});
if (zeroWantLegs.length) {
  log(`${zeroWantLegs.length} zero-want leg(s) — retrying once before recording a gap`);
  const retried = await runMineLegs(zeroWantLegs, true);
  retried.forEach((m, i) => {
    const leg = (m && { path: m.path, family: m.family }) || zeroWantLegs[i];
    const r = m && m.result;
    if (!r) {
      mining_gaps.push({ path: leg.path, family: leg.family, gap: "worker collapsed on the zero-want retry; leg produced nothing" });
      return;
    }
    if (r.read_complete !== true) {
      mining_gaps.push({ path: leg.path, family: leg.family, gap: `incomplete read on retry: ${r.read_note || "no detail given"}` });
    }
    if (r.wants.length === 0) {
      mining_gaps.push({
        path: leg.path,
        family: leg.family,
        gap: "zero wants twice from this transcript — confirm the conversation genuinely voices none before treating the exclusion diff as complete",
      });
      return;
    }
    for (const w of r.wants) minedWants.push({ ...w, family: leg.family });
  });
}
log(
  `${minedWants.length} want(s) mined (pre-dedup, both families)` +
    (mining_gaps.length ? `; ${mining_gaps.length} leg gap(s) — the pool is NOT the two-family union` : ""),
);

// Stage 2 — DIFF, blind and cross-family. Each family's differ sees only the
// two bare lists — no miner reasoning, no draft context — because a checker
// handed reasoning alongside the lists rationalises a mismatch. The exclusion
// diff is the UNION of both unmatched sets: dedup and disposition belong to
// the adjudicating session, which can see the register. A collapsed differ is
// recorded as a gap, never silently dropped.
phase("diff");
const diff_gaps = [];
const exclusion_diff = [];
const diff_concerns = [];
if (minedWants.length === 0) {
  // A diff over an empty pool can only return a clean-looking nothing; record
  // the vacuity instead of dispatching it.
  diff_gaps.push({
    family: "both",
    gap: "no mined wants reached the diff — the exclusion diff is vacuous until the mining gaps above are resolved or a wantless conversation is confirmed",
  });
  log("diff skipped: no mined wants — recorded as a gap, not a clean diff");
} else {
  const diffResults = await parallel(
    ["gpt", "claude"].map((family) => () => {
      dispatching(`diff:${family}`, "re-briefed", "the differ sees only the two bare lists by design: a miner continuing into the diff would carry the reasoning that rationalises a mismatch");
      return agent(diffPrompt(minedWants, commitments), {
        ...FAMILY[family],
        ...DIFF_OPTS[family],
        schema: diffSchema,
        label: `diff:${family}`,
      }).then((r) => ({ family, result: r }));
    }),
  );
  diffResults.forEach((d, i) => {
    const family = d ? d.family : ["gpt", "claude"][i];
    const r = d && d.result;
    if (!r) {
      diff_gaps.push({ family, gap: "differ collapsed; this family's diff is missing" });
      return;
    }
    // Commentary about the question itself stays out of the want list: the
    // exclusion diff carries only quoted human wants.
    for (const u of r.unmatched) exclusion_diff.push({ ...u, family });
    if ((r.out_of_scope_concerns || "").trim()) {
      diff_concerns.push({ family, concern: r.out_of_scope_concerns.trim() });
    }
  });
  log(
    `${exclusion_diff.length} unmatched want(s) across both families` +
      (diff_concerns.length ? `; ${diff_concerns.length} differ concern(s) about the question — read before adjudicating` : "") +
      (diff_gaps.length ? `; ${diff_gaps.length} differ gap(s) — the diff is NOT cross-family` : ""),
  );
}

// Stage 3 — HOLLOW. BOTH families judge every in-scope line, verdicts
// reconciled conservatively by the session: hollow if either family says so.
// Per-line single-family judgement was rejected — it hands each line one
// family's blind spots, which is what the cross-family design exists to avoid.
// Each judge sees the WHOLE register and asks whether the cheapest compliant
// implementation survives all lines together: an interlocked register's lines
// deliberately bind each other, so a complaint the sibling lines already
// answer is a covered_by_siblings record, not a hollow verdict.
phase("hollow");
log(`hollow-testing ${hollowScope.length}/${commitments.length} line(s), both families each`);
const hollowLegs = [];
for (const line of hollowScope) {
  for (const family of ["gpt", "claude"]) {
    hollowLegs.push({ line, family });
  }
}
const hollowResults = await parallel(
  hollowLegs.map((leg) => () => {
    dispatching(`hollow:${leg.family}:${leg.line.id}`, "re-briefed", "each line is judged fresh by both families against the whole register; the hollow test runs on a stronger tier than the mining, so no miner could continue into it");
    return agent(hollowPrompt(leg.line, commitments), {
      ...FAMILY[leg.family],
      ...HOLLOW_OPTS[leg.family],
      schema: hollowSchema,
      label: `hollow:${leg.family}:${leg.line.id}`,
    }).then((r) => ({ ...leg, verdict: r }));
  }),
);
// One entry per line×family; a null verdict is a check still owed, never a
// pass — the session treats a one-family line as degraded and a no-family
// line as unaudited.
const hollow_verdicts = hollowLegs.map((leg, i) => {
  const r = hollowResults[i];
  const v = r && r.verdict;
  return v
    ? {
        id: leg.line.id,
        family: leg.family,
        cheapest_implementation: v.cheapest_implementation,
        hollow: v.hollow,
        covered_by_siblings: (v.covered_by_siblings || "").trim() || null,
        reason: v.reason,
        out_of_scope_concerns: (v.out_of_scope_concerns || "").trim() || null,
      }
    : {
        id: leg.line.id,
        family: leg.family,
        cheapest_implementation: null,
        hollow: null,
        covered_by_siblings: null,
        reason: "worker produced no verdict; this family's check is still owed, not passed",
        out_of_scope_concerns: null,
      };
});

const hollowLines = new Set(hollow_verdicts.filter((v) => v.hollow === true).map((v) => v.id));
const owedLines = new Set(hollow_verdicts.filter((v) => v.hollow === null).map((v) => v.id));
log(
  `${hollowLines.size}/${hollowScope.length} line(s) judged hollow by at least one family` +
    (owedLines.size ? `; ${owedLines.size} line(s) carry an owed check (worker collapse)` : ""),
);
const flagged = hollow_verdicts.filter((v) => v.out_of_scope_concerns);
if (flagged.length) {
  log(`${flagged.length} verdict(s) carry a concern about the question itself — read these before acting`);
}

return {
  mined_wants: minedWants,
  mining_gaps,
  exclusion_diff,
  diff_concerns,
  diff_gaps,
  hollow_verdicts,
};
