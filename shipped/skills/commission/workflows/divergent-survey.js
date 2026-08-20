// divergent-survey — blind divergent survey of approaches to one design
// problem: survey the published theory of the outcome, then propose several
// genuinely different approaches split across both model families — half
// seeded with disjoint slices of the research, half blank-slate — each
// abandoning a named assumption of the conventional route, cross-critique
// with the opposite family, then surface a RANKED comparison (never a single
// winner) for the human and main agent to decide together.

// Worker policy — declared once per family; every agent() call spreads its
// family's pins. Critique and
// synthesis work over a decided table, so they hold the resting level, with
// Claude's adversarial critique leg priced at high (references/
// effort-calibration.md). The GPT side pins the flagship: every GPT leg
// here researches, critiques, or synthesises — judgement work, not the
// collection floor. Pin model and effort together on every call: an unpinned
// call has no model to ride.
const FAMILY = {
  gpt: { model: "gpt-5.6-sol", effort: "medium" },
  claude: { model: "claude-opus-5", effort: "high" },
};

// Run this when an important part's option space has not been walked, or when the
// human has offered a specific approach where only an outcome was required (anchor
// risk). It widens the table before commitment.
//
// The SHAPE of an approach carries no citation gate — proposers push for divergence,
// and requiring sourced precedent for the idea itself would suppress the option space
// this exists to widen. The CLAIMS an approach makes about how it performs are gated
// as the schemas require.
//
// Keep proposers blind to the anchor with both controls:
//   1. The BRIEF the main session passes here states the OUTCOME in observable terms
//      and the hard EXTERNAL constraints only. It must NOT name the proposed approach,
//      its vocabulary, or anything that betrays the expected answer. Building that
//      anchor-free brief is the main session's job (see references/dispatched-agents.md).
//   2. Workers have filesystem access, so place them in an isolated empty
//      directory outside the project so a proposer has no incumbent to find.
//      (See references/ensemble-workflows.md.)
// The research stage does not weaken the blindness: researchers get the same
// anchor-free brief and survey what is established for outcomes of this kind — a
// researcher independently surfacing the same public tool the human named is
// convergence signal, not an anchor leak.
//
// INPUT (pass as the run's args):
//   {
//     "brief": {
//       "outcome":     "what the finished thing must let someone do, in observable terms — anchor-free, naming NO tool/approach",
//       "constraints": "hard external facts that bound any approach (integrations, environments, limits)",
//       "importance":  "why this part is load-bearing (optional; scales how hard critics push)"
//     },
//     "count":  4,            // how many divergent approaches (default 4; even keeps the family split balanced)
//     "angles": ["...", ...]  // optional divergence stances; sensible defaults below
//   }
//
// OUTPUT (the run's result): { brief, angles, research[], approaches[], syntheses[] }.
// The main session reads this, relays the ranked options + decisive questions to the
// human, and they choose. The research digest is worth carrying into that discussion.
// Nothing is written to disk here.

const brief = (args && args.brief) || {};
const count = Number(args && args.count) || 4;

// Divergence stances — postures that spread the search across the design space so the
// alternatives do not collapse into four flavours of one idea. They are not properties
// the chosen approach must have; they exist only to force genuinely different shapes.
const DEFAULT_ANGLES = [
  "minimise moving parts — the simplest thing that could deliver the outcome, accepting less flexibility for far less to build, run and reason about",
  "lean on something that exists — find a library, service or off-the-shelf tool that already does most of this, and assess the fit and the lock-in honestly",
  "build for a moving target — assume the outcome will keep changing; favour the approach that is cheapest to reshape later, even if heavier now",
  "invert the obvious — deliberately abandon the single most load-bearing assumption of the conventional approach and see what becomes possible",
];
const angles =
  Array.isArray(args && args.angles) && args.angles.length
    ? args.angles
    : DEFAULT_ANGLES;

// 50/50 family split for proposers: even indices propose on the GPT side, odd
// on Claude, so
// neither family dominates the idea pool. The critic is always the OTHER family, so no
// approach is ever judged only by its own kind.
const proposerFamily = (i) => (i % 2 === 0 ? "gpt" : "claude");
const opposite = (family) => (family === "gpt" ? "claude" : "gpt");

// Proposing explores a deliberately open option space — the named residue that steps
// the GPT side above its declared default. The Claude proposers also step UP a tier: the
// surviving proposals feed a commission — intent text that steers an entire build —
// so the drafting leg runs the top Claude tier (references/model-selection.md —
// leverage prices the top tier). Critique and synthesis stay on the declared
// defaults deliberately, not as an economy: a reviewer below the drafting tier
// previews the weakest reader who will execute the finished text.
const PROPOSE_OPTS = {
  gpt: { effort: "high" },
  claude: { model: "claude-fable-5", effort: "high" },
};

// Stage 0 surveys the published theory of the OUTCOME — how outcomes of this kind
// are achieved well, and where common approaches fail — before any approach exists.
// Two complementary questions, so the digest carries both the attractors and the
// repellers of the design space. Research runs on the GPT side with
// web-search tool grants, and on the declared default level: the
// survey is bounded retrieval whose digest informs proposing without itself being
// the proposal.
const RESEARCH_QUESTIONS = [
  {
    kind: "technique",
    question:
      "What techniques, architectures, or established practice exist for achieving this kind of outcome well — the theory behind the objective?",
  },
  {
    kind: "failure-mode",
    question:
      "What failure modes, pitfalls, and critiques are documented for common approaches to this kind of outcome — where does practice actually go wrong?",
  },
];

// Keep reply schemas an object with additionalProperties:false and every
// property required, with explicit "empty when none" conventions — a worker
// compelled to emit every declared property must have an honest blank
// available, and a null result can then only mean worker collapse.
const researchSchema = {
  type: "object",
  additionalProperties: false,
  required: ["findings"],
  properties: {
    findings: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["name", "summary", "design_implication", "source"],
        properties: {
          name: { type: "string" },
          summary: { type: "string" },
          design_implication: { type: "string" },
          // A named, checkable source — or an honest "parametric" when the
          // strongest material is model knowledge rather than a document.
          source: { type: "string" },
        },
      },
    },
  },
};

const approachSchema = {
  type: "object",
  additionalProperties: false,
  required: [
    "approach_name",
    "one_line",
    "outcome_restated",
    "abandoned_assumption",
    "how_it_works",
    "easier",
    "harder",
    "wins_when",
    "sacrifices",
    "evidence",
  ],
  properties: {
    approach_name: { type: "string" },
    one_line: { type: "string" },
    // Forces restate-first: prove the outcome was understood free of any solution.
    outcome_restated: { type: "string" },
    // The falsifiable divergence test: name the conventional assumption this abandons.
    abandoned_assumption: { type: "string" },
    how_it_works: { type: "array", items: { type: "string" } },
    easier: { type: "array", items: { type: "string" } },
    harder: { type: "array", items: { type: "string" } },
    wins_when: { type: "string" },
    sacrifices: { type: "string" },
    evidence: { type: "array", items: { type: "string" } },
  },
};

const critiqueSchema = {
  type: "object",
  additionalProperties: false,
  required: ["verdict", "is_reskin", "fatal_flaws", "grounding_gaps", "missing", "fixes", "out_of_scope_concerns"],
  properties: {
    verdict: { type: "string", enum: ["keep", "fix", "drop"] },
    // Divergence check: a candidate that just re-dresses the conventional route is theatre.
    is_reskin: { type: "boolean" },
    fatal_flaws: { type: "array", items: { type: "string" } },
    grounding_gaps: { type: "array", items: { type: "string" } },
    missing: { type: "array", items: { type: "string" } },
    fixes: { type: "array", items: { type: "string" } },
    // Somewhere to object to the QUESTION rather than the approach — an outcome
    // brief that is itself wrong or contradictory, or research findings that look
    // fabricated. Required with an explicit empty-string convention: a
    // schema-bound worker is compelled to emit every declared property, so an
    // optional field
    // would invite invented content over an honest blank.
    out_of_scope_concerns: { type: "string" },
  },
};

// Synthesis RANKS and surfaces decisive questions; it does NOT pick a winner. The
// human and main agent decide — this is the "surface for joint decision" contract.
const synthesisSchema = {
  type: "object",
  additionalProperties: false,
  required: [
    "outcome_restated",
    "ranked_options",
    "decisive_questions",
    "strongest_elements",
    "convergence_note",
    "open_questions",
  ],
  properties: {
    outcome_restated: { type: "string" },
    ranked_options: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["approach_name", "rank", "why_here", "key_tradeoff", "wins_when"],
        properties: {
          approach_name: { type: "string" },
          rank: { type: "integer" },
          why_here: { type: "string" },
          key_tradeoff: { type: "string" },
          wins_when: { type: "string" },
        },
      },
    },
    // What the human/agent must answer to choose — the survey's job is to sharpen these.
    decisive_questions: { type: "array", items: { type: "string" } },
    // Ideas worth grafting into the chosen route whichever option wins, each
    // attributed to the approach it came from.
    strongest_elements: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["element", "from_approach"],
        properties: {
          element: { type: "string" },
          from_approach: { type: "string" },
        },
      },
    },
    // Did the approaches converge? Strong convergence is quiet confirmation the
    // conventional route may genuinely be right; wide divergence is itself a finding.
    convergence_note: { type: "string" },
    open_questions: { type: "array", items: { type: "string" } },
  },
};

const briefBlock = JSON.stringify(brief, null, 2);

const researchPrompt = (question) =>
  `You are surveying established knowledge to inform approaches to a software design
problem. You are NOT proposing an approach; a later stage does that.

Outcome and constraints:
${briefBlock}

Your survey question: ${question}

Search the live web for what is genuinely established on this question — published
techniques, documented practice, research, post-mortems. Return three to six findings.
Each must be a distinct idea rather than a variation on another, and self-contained
enough to hand to a designer alone: what it is (\`summary\`), what it implies for an
approach to this outcome (\`design_implication\`), and where it comes from (\`source\` —
a named, checkable source; prefer document-backed findings, and write "parametric" only
when genuinely established material lacks a citable document — never an invented
citation). Budget the retrieval mechanically: start with one broad search and stop at
three searches in total, searching again only when the results so far cannot support
three well-sourced findings — a small set of well-sourced findings beats exhaustive
coverage. Do not try to find out what approach was already chosen for this problem;
survey what is established for outcomes of this kind.`;

const findingsBlock = (findings) =>
  `

Research findings — your slice of a survey of the published theory of this outcome
(other proposers hold other slices):
${JSON.stringify(findings, null, 2)}

Treat the findings as raw material, not requirements: build on them, adapt them, or
argue against them, and where one shapes a claim, cite it in \`evidence\`. Your approach
must still genuinely follow your stance.`;

const proposePrompt = (angle, findings) =>
  `You are proposing an approach to a software design problem from its outcome.

Outcome and constraints (deliberately minimal — you have not been shown any
incumbent or already-proposed approach):
${briefBlock}

Take this design stance and commit to it: ${angle}${findings ? findingsBlock(findings) : ""}

First, restate the outcome in your own words, free of any particular solution — this is
\`outcome_restated\`. Then propose ONE concrete approach that genuinely follows the stance.
This is divergent ideation: favour a distinctive, fully-committed shape over a safe
consensus one — a later stage reconciles the alternatives, so do not hedge to the middle.
You have deliberately not been shown whatever approach was already proposed for this
problem; do not reconstruct or default to it. An off-the-shelf library, service or tool
that already delivers the outcome IS a legitimate approach, often the best one; assess its
fit and lock-in honestly rather than reflexively building from scratch. Name the single
conventional assumption your approach abandons (\`abandoned_assumption\`) — if you cannot
name one, your approach is not divergent enough. Give how it works, what it makes easier,
what it makes harder, what must be true for it to be the right call, and what it uniquely
sacrifices. Ground these trade-off claims: attach evidence, quote a source where one
applies, treat "couldn't determine" as a valid answer rather than guessing, and separate
what you observe from what you infer. Be specific to THIS outcome; avoid generic
architecture advice.

Reply with a single JSON value conforming to the schema. Reason first if you need to, but the
JSON value comes last: the final non-whitespace characters of your message must be the JSON
value itself — no prose, no code fence, no example after it. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

const critiqueFindingsBlock = (findings) =>
  `

Research findings the proposer was given (verify any \`evidence\` that cites them —
a citation that misstates or overstretches its finding is a grounding gap):
${JSON.stringify(findings, null, 2)}`;

const critiquePrompt = (approach, findings) =>
  `You are independently reviewing one proposed approach to a software design problem. Be
sceptical, not agreeable. Find why it would FAIL or mislead in practice — do not summarise
its strengths back to it.

Outcome and constraints:
${briefBlock}

Proposed approach:
${JSON.stringify(approach, null, 2)}${findings ? critiqueFindingsBlock(findings) : ""}

Judge two things. (1) DIVERGENCE: is this a genuinely different approach, or just the
conventional route re-dressed (set \`is_reskin\`)? An approach whose \`abandoned_assumption\`
is empty, cosmetic, or not actually abandoned is a reskin. (2) SOUNDNESS: fatal flaws that
would sink it; claims that rest on nothing checkable (\`grounding_gaps\`); load-bearing
considerations it MISSES. Give concrete fixes. Then a verdict: "keep" (sound and divergent),
"fix" (salvageable), or "drop" (reskin, or the shape is wrong). Default to "fix"/"drop" if
uncertain — an approach that survives real scrutiny is the point.

If something is wrong with what you were GIVEN rather than the approach — the outcome brief
is itself contradictory or wrong, or a research finding looks fabricated — say so in
\`out_of_scope_concerns\`; use an empty string when you have nothing, and never pad it.

Reply with a single JSON value conforming to the schema. Reason first if you need to, but the
JSON value comes last: the final non-whitespace characters of your message must be the JSON
value itself — no prose, no code fence, no example after it. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Fan-in guard: the synthesis prompt is the SUM of every approach/critique pair plus
// the research digest, and one worker fed tens of KB degrades to retries, then null.
// Ranking is a global judgement — every option on the table at once — so the guard is
// distillation, not tree-reduce: over the bound, each pair is first compressed into a
// faithful synopsis and the synthesis ranks the synopses. Under the bound (the usual
// case at the default count) the full pairs go through untouched.
// Measured in string length — a close proxy for bytes on this mostly-ASCII JSON,
// and the bound is a degradation heuristic, not a protocol limit.
const SYNTHESIS_INPUT_BOUND = 32_000;

// The synopsis keeps every judgement-bearing category the synthesis ranks on —
// compressed within each field, but no category dropped, so ranking synopses
// never silently loses the critique's teeth or the approach's evidence.
const synopsisSchema = {
  type: "object",
  additionalProperties: false,
  required: [
    "approach_name",
    "one_line",
    "abandoned_assumption",
    "essence",
    "key_tradeoffs",
    "wins_when",
    "sacrifices",
    "evidence",
    "critique_verdict",
    "is_reskin",
    "fatal_flaws",
    "grounding_gaps",
    "missing",
  ],
  properties: {
    approach_name: { type: "string" },
    one_line: { type: "string" },
    abandoned_assumption: { type: "string" },
    essence: { type: "string" },
    key_tradeoffs: { type: "array", items: { type: "string" } },
    wins_when: { type: "string" },
    sacrifices: { type: "string" },
    evidence: { type: "array", items: { type: "string" } },
    critique_verdict: { type: "string" },
    is_reskin: { type: "boolean" },
    fatal_flaws: { type: "array", items: { type: "string" } },
    grounding_gaps: { type: "array", items: { type: "string" } },
    missing: { type: "array", items: { type: "string" } },
  },
};

const synopsisPrompt = (pair) =>
  `Compress this proposed design approach and its independent critique into a faithful
synopsis for a ranking stage. Every schema field maps to a category of the original —
compress within each category, never by dropping one: keep the approach's shape, what it
abandons, its evidence and sacrifices, and the critique's verdict with its fatal flaws,
grounding gaps, and missed considerations. Do not soften the critique or advocate for the
approach.

${JSON.stringify(pair, null, 2)}

Reply with a single JSON value conforming to the schema. Reason first if you need to, but the
JSON value comes last: the final non-whitespace characters of your message must be the JSON
value itself — no prose, no code fence, no example after it. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

const synthesisPrompt = (pairs) =>
  `You have several alternative approaches to the SAME design problem, each with an
independent critique. Produce a RANKED comparison for a human and an AI agent to decide
between together. You are NOT choosing for them — you are sharpening the choice.

Drop or down-rank approaches the critique marked as reskins. Rank the survivors and for
each give why it sits where it does, its key trade-off, and what must be true for it to be
the right call; where the research digest below speaks to an approach's claims, judge them
against it. Then list the \`decisive_questions\` — the few things the human and agent
must answer to pick — and the \`strongest_elements\`: ideas worth grafting into the chosen
route whichever option wins, each attributed to the approach it came from. Finish with a
\`convergence_note\`: did the approaches converge (quiet signal the conventional route may
be right) or diverge widely (a finding in itself)? Restate the outcome they all serve. Do
not recommend one as the answer.

Outcome and constraints:
${briefBlock}

Research digest (published theory of the outcome; some approaches drew on slices of it):
${JSON.stringify(research, null, 2)}

Alternatives and their critiques:
${JSON.stringify(pairs, null, 2)}

Reply with a single JSON value conforming to the schema. Reason first if you need to, but the
JSON value comes last: the final non-whitespace characters of your message must be the JSON
value itself — no prose, no code fence, no example after it. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Stage 0: survey the theory before any approach exists. A researcher that
// collapses to null after retries degrades gracefully to a smaller digest;
// both collapsing degrades every proposer to blank-slate.
phase("research");
const research = (
  await parallel(
    RESEARCH_QUESTIONS.map(({ kind, question }) => () =>
      agent(researchPrompt(question), {
        ...FAMILY.gpt,
        schema: researchSchema,
        label: `research:${kind}`,
      }).then((r) => (r ? r.findings.map((f) => ({ kind, ...f })) : [])),
    ),
  )
).flat();
log(`${research.length} research findings gathered`);

// Half the proposers work from a disjoint slice of the digest, half from the
// brief alone — grounded prior art and unanchored invention in one pool, with the
// cross-critique arbitrating between them. Dealing the findings round-robin keeps
// the informed proposers from converging on one discovered pattern. At the default
// even count >= 4 the informed half spans both families (even/odd indices alternate
// family); a smaller or odd count collapses that to one family, so it is logged
// rather than silently assumed. An empty slice (digest smaller than the informed
// half) degrades that slot to blank-slate rather than shipping an empty block.
const informedCount = research.length ? Math.floor(count / 2) : 0;
const researchSlice = (k) => {
  const slice = research.filter((_, j) => j % informedCount === k);
  return slice.length ? slice : null;
};
if (informedCount === 0) {
  log("research digest empty — every proposer runs blank-slate");
} else if (informedCount < 2) {
  log("informed half is a single proposer — research-seeded proposing is one family only this run");
}

// Stage 1+2 as a pipeline: each proposal flows straight into its cross-family critique,
// so a fast proposer is critiqued while a slow proposer is still thinking.
phase("propose+critique");
const items = Array.from({ length: count }, (_, i) => ({
  i,
  angle: angles[i % angles.length],
  family: proposerFamily(i),
  findings: i < informedCount ? researchSlice(i) : null,
}));

const pairs = await pipeline(
  items,
  (item) =>
    agent(proposePrompt(item.angle, item.findings), {
      ...FAMILY[item.family],
      ...PROPOSE_OPTS[item.family],
      schema: approachSchema,
      label: `propose:${item.family}:${item.i}`,
    }),
  (approach, item) => {
    if (!approach) return null; // proposer collapsed after retries; drop this slot
    const critic = opposite(item.family);
    // The critic sees the same findings slice the proposer had, so research-backed
    // evidence is checkable rather than waved through on the citation's say-so.
    return agent(critiquePrompt(approach, item.findings), {
      ...FAMILY[critic],
      schema: critiqueSchema,
      label: `critique:${critic}:${item.i}`,
    }).then((critique) => ({
      angle: item.angle,
      proposer: item.family,
      critic,
      researched: item.findings !== null,
      approach,
      critique,
    }));
  },
);

const valid = pairs.filter(Boolean).filter((p) => p.approach && p.critique);
log(`${valid.length}/${count} approaches survived propose+critique`);
if (valid.length === 0) {
  throw new Error("no approach survived the propose+critique stage");
}
// A critic's objection to the QUESTION is exactly what a clean-looking table
// would otherwise bury — surface it before the verdicts beside it are trusted.
const flagged = valid.filter((p) => (p.critique.out_of_scope_concerns || "").trim());
if (flagged.length) {
  log(`${flagged.length} critique(s) carry a concern about the brief or inputs themselves — read these before acting on the verdicts: ${flagged.map((p) => p.approach.approach_name).join(", ")}`);
}

// Both families synthesise from everything, so the ranking is not one family's read of
// the table. The main session compares the two rankings when relaying to the human.
// Fan-in guard applied here: over the bound, distil each pair to a synopsis first
// (each distiller reads ONE pair — bounded input by construction), then rank the
// synopses; a pair whose distiller collapses falls back to its full form.
phase("synthesise");
let table = valid;
if (JSON.stringify(valid).length > SYNTHESIS_INPUT_BOUND) {
  log(`pairs exceed ${SYNTHESIS_INPUT_BOUND} chars serialised — distilling before synthesis`);
  const synopses = await parallel(
    valid.map((pair, k) => () =>
      agent(synopsisPrompt(pair), {
        // Distillation is compression of a single decided pair, not judgement over
        // the table — but alternate the family per pair so neither family's
        // compression is the only view the rankers ever see.
        ...FAMILY[k % 2 === 0 ? "gpt" : "claude"],
        schema: synopsisSchema,
        label: `distil:${k}`,
      }),
    ),
  );
  // A collapsed distiller must not restore its full oversized pair (that would
  // resurrect the very input the bound exists to prevent); project the pair's
  // ranked-on fields mechanically instead, so the option stays on the table.
  table = synopses.map((s, k) => {
    if (s) return s;
    const { approach, critique } = valid[k];
    return {
      approach_name: approach.approach_name,
      one_line: approach.one_line,
      abandoned_assumption: approach.abandoned_assumption,
      essence: (approach.how_it_works || []).join(" "),
      key_tradeoffs: [...(approach.easier || []), ...(approach.harder || [])],
      wins_when: approach.wins_when,
      sacrifices: approach.sacrifices,
      evidence: approach.evidence || [],
      critique_verdict: critique.verdict,
      is_reskin: critique.is_reskin,
      fatal_flaws: critique.fatal_flaws || [],
      grounding_gaps: critique.grounding_gaps || [],
      missing: critique.missing || [],
    };
  });
}

const syntheses = (
  await parallel(
    ["gpt", "claude"].map((family) => () =>
      agent(synthesisPrompt(table), {
        ...FAMILY[family],
        schema: synthesisSchema,
        label: `synthesise:${family}`,
      }).then((s) => (s ? { family, ...s } : null)),
    ),
  )
).filter(Boolean);

return {
  brief,
  angles,
  research,
  approaches: valid.map(({ angle, proposer, critic, researched, approach, critique }) => ({
    angle,
    proposer,
    critic,
    researched,
    approach,
    critique,
  })),
  syntheses,
};
