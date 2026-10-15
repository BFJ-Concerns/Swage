// blind-rederivation — blind re-derivation of a design from first principles:
// one sustained high-tier designer per chunk, isolated by construction from
// the incumbent design, derives the concerns and the shape from an
// anchor-audited brief alone — later chunks build on earlier blind output as
// a fixed contract with explicit licence to propose amendments — then, when
// the settled design is supplied, both families independently report where
// the blind result converges with it (validation) and diverges from it (the
// findings).

// Worker policy — declared once per family; every agent() call spreads its
// family's pins, and call sites override only where a stage's residue earns
// more, with a comment naming it. The resting posture is the comparison
// stage: judgement over two decided texts, so the GPT side pins the flagship
// at the resting level (review work runs the flagship —
// references/model-selection.md). Pin model and effort together on every
// call: an unpinned call has no model to ride.
export const meta = {
  description:
    "Have blind designers re-derive a design from anchor-free briefs, one sustained designer per chunk, and compare the results against the settled design when one is supplied.",
  argumentSchema: {
    "type": "object",
    "properties": {
      "run_dir": {
        "type": "string",
        "description": "absolute path to an isolated empty directory for this run"
      },
      "domain": {
        "type": "string",
        "description": "the product domain stated generically, with no product name or incumbent vocabulary"
      },
      "premise": {
        "type": "string",
        "description": "the user's core hypothesis, taken as a given design premise"
      },
      "chunks": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "id": {
              "type": "string",
              "description": "stable handle; names design-<id>.md"
            },
            "brief": {
              "type": "string",
              "description": "the anchor-audited brief for this chunk"
            }
          },
          "required": [
            "id",
            "brief"
          ]
        },
        "description": "ordered chunks, one sustained designer each",
        "minItems": 1
      },
      "existing": {
        "type": "string",
        "description": "the settled design text to compare against; omit to run derivation only"
      }
    },
    "required": [
      "run_dir",
      "domain",
      "chunks"
    ]
  },
};

const FAMILY = {
  gpt: { model: "gpt-6.1-sol", effort: "medium" },
  claude: { model: "claude-opus-5-5", effort: "high" },
};

// THE BLINDNESS IS ISOLATION BY CONSTRUCTION, NOT INSTRUCTION.
//   * Every worker is placed in the isolated EMPTY run directory the args
//     name (see references/ensemble-workflows.md) — workers have filesystem
//     access, so an empty working directory
//     means there is no repo, no README, no notes for a designer to find. Contamination is
//     prevented by what the worker cannot reach.
//   * The BRIEF is the other half, and it is the main session's job to audit it before
//     invoking (references/ensemble-workflows.md, brief discipline): the domain stated
//     generically, no product-specific nouns or taxonomies from the incumbent, every
//     requirement worded as a problem to solve rather than a shape to build, and the
//     user's core hypothesis — where there is one — framed as a GIVEN premise so the
//     designer builds on it instead of relitigating it.
//   * The settled design (`existing`), when supplied for comparison, is shown ONLY to the
//     comparison stage, which runs after every derivation completes. It never reaches a
//     designer.
//
// ONE SUSTAINED DESIGNER PER CHUNK, NOT A FAN-OUT.
// A feature-sized subject is one chunk; a project-sized one is split by
// the main session into pieces a designer can hold coherently, run in sequence, each later
// chunk receiving the earlier blind output as its fixed consumer contract — itself
// uncontaminated material — with explicit licence to propose amendments to it in a
// dedicated section rather than silently warping around a plan that did not know this
// chunk would exist.
//
// Designers write their document incrementally, section by section, to a file in the run
// directory. A long single-emission output dies in mid-stream stalls. The structured
// return is deliberately small — the document is the deliverable; the JSON is its index.
//
// INPUT (pass as the run's args):
//   {
//     "run_dir": "/abs/path to an isolated EMPTY directory for this run — designers write and read design documents there",
//     "domain":  "the product domain stated generically — no product name, no incumbent vocabulary",
//     "premise": "optional: the user's core hypothesis, to be taken as a given design premise",
//     "chunks": [                    // ordered; one sustained designer each
//       {
//         "id":    "core",           // stable handle; names the output file design-<id>.md
//         "brief": "the anchor-audited brief for this chunk: the open task shape, and every requirement worded as a need, not a mechanism"
//       }
//     ],
//     "existing": "optional: the settled design text to compare against — never shown to designers. Omit it to run derivation only (the greenfield counterpoint case, where the comparison is the conversation itself)."
//   }
//
// OUTPUT (the run's result): { designs[], comparisons[] }. Each design entry carries the
// document's file path (in the run directory), the designer's own load-bearing choices,
// and any amendments it proposed to earlier chunks. Comparisons (one per family, when
// `existing` was supplied) report convergences, divergences, and open questions. The main
// session relays both to the user — convergence as validation, divergence as findings —
// and the amendments are settled explicitly, never silently absorbed.

const cfg = args || {};
const runDir = (cfg.run_dir || "").trim();
const domain = (cfg.domain || "").trim();
const premise = (cfg.premise || "").trim();
const chunks = Array.isArray(cfg.chunks) ? cfg.chunks : [];
const existing = (cfg.existing || "").trim();
if (!runDir) throw new Error("blind-rederivation needs run_dir — an isolated empty directory for the designers");
if (!domain) throw new Error("blind-rederivation needs a generically-stated domain");
if (chunks.length === 0) throw new Error("blind-rederivation needs at least one chunk");
// The chunk id names the output file, so malformed input fails here, not mid-run.
const seen = new Set();
for (const chunk of chunks) {
  if (!chunk || typeof chunk.id !== "string" || !/^[A-Za-z0-9_-]+$/.test(chunk.id)) {
    throw new Error(`chunk id must be a filename-safe string, got: ${chunk && chunk.id}`);
  }
  if (seen.has(chunk.id)) throw new Error(`duplicate chunk id: ${chunk.id}`);
  seen.add(chunk.id);
  if (typeof chunk.brief !== "string" || !chunk.brief.trim()) {
    throw new Error(`chunk "${chunk.id}" needs a non-empty brief`);
  }
}

// Small structured index over the written document — the document itself is the
// deliverable and stays on disk.
const designSchema = {
  type: "object",
  additionalProperties: false,
  required: ["output_file", "summary", "load_bearing_choices", "amendments_to_prior"],
  properties: {
    output_file: { type: "string" },
    summary: { type: "string" },
    // The designer's own judgement calls — the comparison stage and the human read
    // these as the places the design would most repay scrutiny.
    load_bearing_choices: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["choice", "why"],
        properties: {
          choice: { type: "string" },
          why: { type: "string" },
        },
      },
    },
    // Non-empty only for chunks after the first: proposed changes to the earlier
    // blind output this chunk was building on.
    amendments_to_prior: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["target", "proposal", "why"],
        properties: {
          target: { type: "string" },
          proposal: { type: "string" },
          why: { type: "string" },
        },
      },
    },
  },
};

const comparisonSchema = {
  type: "object",
  additionalProperties: false,
  required: ["convergences", "divergences", "open_questions", "overall"],
  properties: {
    // Where blind and settled agree — independent validation of the settled choice.
    convergences: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["point", "existing_evidence", "blind_evidence"],
        properties: {
          point: { type: "string" },
          existing_evidence: { type: "string" },
          blind_evidence: { type: "string" },
        },
      },
    },
    // Where they part — each divergence names the assumption underneath it, because
    // the unexamined assumption is what this workflow exists to surface.
    divergences: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["point", "existing_position", "blind_position", "underlying_assumption", "significance"],
        properties: {
          point: { type: "string" },
          existing_position: { type: "string" },
          blind_position: { type: "string" },
          underlying_assumption: { type: "string" },
          significance: { type: "string", enum: ["structural", "material", "minor"] },
        },
      },
    },
    open_questions: { type: "array", items: { type: "string" } },
    overall: { type: "string" },
  },
};

const premiseBlock = premise
  ? `\nTake the following as a GIVEN design premise, whether or not you would have chosen it
yourself — build on it rather than debating it:\n${premise}\n`
  : "";

// Prior documents are handed over by file path, not inlined: the script sandbox has
// no filesystem, but every worker shares the run directory as its working
// directory — and reading a document is better
// than receiving it pasted, since a whole design does not belong inside a prompt.
const priorBlock = (priors) =>
  priors.length
    ? `\nAn earlier stage of this derivation has already produced design document(s) in the
current directory — read each in full before designing:\n${priors
        .map((p) => `- ${p.output_file} (${p.summary})`)
        .join("\n")}\nTreat them as your FIXED consumer contract: design for them, do not silently
redesign them. Where your part genuinely needs one of them to be different, propose the
change in a dedicated "Proposed amendments" section of your document (and in
\`amendments_to_prior\`) rather than warping your design around the mismatch — the earlier
document was written without knowing yours would exist.\n`
    : "";

const designPrompt = (chunk, priors) =>
  `You are designing a system from first principles. You have deliberately been given no
existing product, codebase, or prior art for this problem — that is the point of the
exercise, so do not research existing products, do not model your design on ones you can
recall, and do not ask questions. Where you must make a judgement call, make it and record
it.

The domain, stated generically:
${domain}
${premiseBlock}${priorBlock(priors)}
Your design task:
${chunk.brief}

Derive the concerns before designing for them: begin from what this domain inherently
requires, decide which problems the design must solve, and only then shape the design that
solves them. Favour the design you actually believe in over a safe consensus shape.

Write your design as a Markdown document at \`design-${chunk.id}.md\` in the current
directory — and write it INCREMENTALLY: create the file first, then append one section at a
time as you complete it, re-reading what you have written before each new section. Never
compose the whole document in your head and emit it in one go. End the document with a
section naming your load-bearing choices — the judgement calls the rest of the design
hangs on — each with its rationale; mirror them in \`load_bearing_choices\`. Return the
file path, a short summary, and (if you were given earlier design documents) any
amendments you proposed to them.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — every JSON value in your reply is checked against the schema and the LAST valid one is read, so use exactly one fence and put nothing schema-shaped after it. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

const comparisonPrompt = (designs) =>
  `You are comparing two independently produced designs for the same system: a SETTLED
design that a team is working from, and a BLIND design derived from first principles by a
designer who never saw the settled one. Neither is presumed right. Your report feeds a
conversation between the team and their planning agent.

The settled design:
<settled>
${existing}
</settled>

The blind design document(s) are on disk in the current directory — read each in full:
${designs.map((d) => `- ${d.output_file} (${d.summary})`).join("\n")}

Report three things, each grounded in quoted evidence from both texts:
- \`convergences\`: where the blind design independently arrived at the settled design's
  choice. These validate the settled choice — say what each side's text shows.
- \`divergences\`: where they part. For each, state both positions, name the
  \`underlying_assumption\` — the question one design asked and the other never did, or
  answered differently — and rate its significance: "structural" reshapes the design,
  "material" changes real behaviour, "minor" is taste. Divergence is a finding, not a
  defect: do not pick a winner, and do not soften a structural divergence into diplomacy.
- \`open_questions\`: what the comparison cannot settle from the texts alone — the
  questions the team must answer to choose between positions.

The blind documents name their own load-bearing choices; check each against the settled
design and make sure every one is accounted for in your convergences or divergences.

Reply with a single JSON value conforming to the schema. Reason first if you need to, then give the JSON inside a single \`\`\`json fenced code block — every JSON value in your reply is checked against the schema and the LAST valid one is read, so use exactly one fence and put nothing schema-shaped after it. A reply that is the bare JSON value alone, with no prose at all, also works. Never restate the schema or show
a sample answer; the only JSON value in your reply is your actual answer.`;

// Stage 1 — derive, one sustained designer per chunk, in order. Sequential by design:
// each later chunk builds on the earlier blind output, so there is nothing to fan out.
// The designer runs the top Claude tier at high effort: a whole-design derivation is
// drafting whose output steers a build or a pivot — leverage prices the top tier
// (references/model-selection.md) — and the open design space is the named residue
// behind high (references/effort-calibration.md).
// Every dispatch records on the run journal whether it continued a held
// worker or re-briefed a fresh one, and why — the choice is this script's,
// made per stage (the shipped set's rule: continue where a later turn
// profits from a worker's accumulated context, re-brief where independence
// is the point or the envelope differs).
const dispatching = (label, mode, reason) => log(`[dispatch] ${label}: ${mode} — ${reason}`);

phase("derive");
const designs = [];
for (const chunk of chunks) {
  log(`deriving chunk "${chunk.id}" (${designs.length ? "building on " + designs.map((d) => d.id).join(", ") : "first chunk"})`);
  dispatching(`design:${chunk.id}`, "re-briefed", designs.length
    ? "a later chunk takes the earlier blind output as a fixed contract read from disk, not as its own memory of deriving it: a designer continuing its own session would warp the new chunk around reasoning the contract never recorded"
    : "the first designer opens blind by construction in the empty run directory");
  const result = await agent(designPrompt(chunk, designs), {
    // Top Claude tier: leverage prices the top tier for whole-design drafting.
    model: "claude-fable-5-1",
    effort: "high",
    cwd: runDir,
    schema: designSchema,
    label: `design:${chunk.id}`,
  });
  if (!result) throw new Error(`designer for chunk "${chunk.id}" produced no result`);
  // The path is fixed by the chunk id, not taken from the worker's report — later
  // stages read the file, so a misreported path would silently blind them.
  designs.push({ id: chunk.id, ...result, output_file: `design-${chunk.id}.md` });
}

// Stage 2 — compare, only when the settled design was supplied. Both families read
// the same pair independently, so the convergence/divergence read is not one
// family's judgement of another family's output. Comparators read the blind
// documents from disk — two fixed documents per worker, bounded by construction.
let comparisons = [];
if (existing) {
  phase("compare");
  comparisons = (
    await parallel(
      ["gpt", "claude"].map((family) => () => {
        dispatching(`compare:${family}`, "re-briefed", "the comparison is a fresh cross-family read of two finished texts; a designer comparing its own document would defend it");
        return agent(comparisonPrompt(designs), {
          ...FAMILY[family],
          cwd: runDir,
          schema: comparisonSchema,
          label: `compare:${family}`,
        }).then((c) => (c ? { family, ...c } : null));
      }),
    )
  ).filter(Boolean);
  // The cross-family read is the point: a lost comparator is a failed run, not a
  // quietly thinner result the main session would read as complete.
  if (comparisons.length < 2) {
    throw new Error(
      `comparison incomplete: only ${comparisons.map((c) => c.family).join(", ") || "no"} family returned — re-run before relying on the comparison`,
    );
  }
} else {
  log("no settled design supplied — derivation only; the comparison is the conversation's job");
}

return { designs, comparisons };
