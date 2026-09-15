// triage-wave — open a fresh assembly run: cheap workers sweep the
// commission-versus-codebase delta, propose decomposition criteria and
// obligation entries under them, and a reducer merges the proposals into
// one document for the orchestrator's judgement. Proposes only — never
// writes the ledger header or the obligation records.

export const defaults = {
  model: "gpt-5.6-luna",
  effort: "low",
};

// INPUT (workflow args):
//   repo:            absolute path to the code repository
//   commissionPath:  absolute path to the annexe README
//   spinePath:       absolute path to build/spine.md
//   retiredPath:     absolute path to build/retired.md
//   ledgerPath:      absolute path to build/ledger.md (the ledger header)
//   obligationsDir:  absolute path to build/obligations/ (the per-obligation
//                    records; may hold maestro's seed)
//   tasksPath:       absolute path to the annexe's TASKS.md (the decided-journey queue)
//   outPath:         absolute path the merged proposal document is written to
//   waves:           optional worker count (default 4)

const { repo, commissionPath, spinePath, retiredPath, ledgerPath, obligationsDir, tasksPath, outPath } = args;
if (!tasksPath) throw new Error("tasksPath is required: the annexe's TASKS.md must feed the wave");
if (!obligationsDir) throw new Error("obligationsDir is required: the wave reads build/obligations/ to avoid duplicating seeded records");
const waves = Number(args.waves) || 4;

// Every dispatch records on the run journal whether it continued a held
// worker or re-briefed a fresh one, and why — the choice is this script's,
// made per stage (the shipped set's rule: continue where a later turn
// profits from a worker's accumulated context, re-brief where independence
// is the point or the envelope differs).
const dispatching = (label, mode, reason) => log(`[dispatch] ${label}: ${mode} — ${reason}`);

phase("propose");

const proposalSchema = {
  type: "object",
  additionalProperties: false,
  required: ["criteria", "criteria_why", "obligations", "queue_strikes"],
  properties: {
    criteria: { type: "string" },
    criteria_why: { type: "string" },
    queue_strikes: { type: "array", items: { type: "string" } },
    obligations: {
      type: "array",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["slug", "must_be_true", "proof", "source", "spine_status", "fan_out"],
        properties: {
          slug: { type: "string" },
          must_be_true: { type: "string" },
          proof: { type: "string" },
          source: { type: "string" },
          spine_status: { type: "string", enum: ["covered", "needs-spine", "unsure"] },
          fan_out: { type: "string" },
        },
      },
    },
  },
};

const proposePrompt = (i) => `You are proposing the work decomposition for a build run. Read the contract at ${commissionPath} (the What is the destination), then survey the repository at ${repo} to judge what already exists. Also read: ${spinePath} (the exemplar patterns builders may extend — note each entry's name), ${retiredPath} (capabilities deliberately not coming back — never propose these), ${ledgerPath} (the ledger header) with the per-obligation records in ${obligationsDir} (obligations already seeded — do not duplicate them; sharpen or extend around them), and ${tasksPath} (the decided-journey queue — skip items already marked "(→ <slug>)": their obligation exists, whatever its status. Propose an obligation for each remaining item tagged "(journey)" unless an obligation record of any status already covers its work or the item carries an active "(blocked on ...)" marker (its blocker still stands — skip it); for an item carrying no kind tag, propose only when it is plainly journey-shaped and small — untagged items otherwise await commission triage; items tagged "(structural)" are the structural phase's, so skip them; the orchestrator judges dispositions at landing — small items it will fix directly rather than convert — so within those bounds propose rather than filter).

First choose your DECOMPOSITION CRITERIA: the axis that cleaves this project's remaining work at its natural joints — by capability, by user journey, by surface, by data flow; by-file is almost never right. You are proposer ${i + 1} of ${waves}: if an obvious axis exists, consider whether a different one reveals work the obvious axis hides — disagreement between proposers is wanted.

Then propose OBLIGATIONS under your criteria: each an outcome that must be true and provable before the build is complete, in the contract's own terms. For each: slug (kebab-case), must_be_true (verifiable, one or two sentences), proof (how it would be proven at the project's real consumer — the journey or command, and the discriminating observation separating the built behaviour from its absence), source (the contract line or seeded entry it derives from, quoted briefly), spine_status ("covered" naming the exemplar in parentheses, "needs-spine", or "unsure"), fan_out (the independent slices the obligation's work divides into for parallel builders — name each slice and the seam that keeps their edits apart, judged from the actual code layout; never split a capability from its thinnest real consumer across slices, and write "none" when the work is one piece or the slices would edit the same files).

Propose what the delta actually needs — no padding obligations for completeness. Alongside the obligations, list queue_strikes: queue items whose work a proven obligation record already covers — name the item and the proven record's slug; an implementation with no proven record is not a strike, it is an obligation. Empty when none. Reply with the JSON last.`;

const proposals = (
  await parallel(
    Array.from({ length: waves }, (_, i) => () =>
      {
        dispatching(`propose:${i}`, "re-briefed", "each proposer is an independent read of the same delta — disagreement between them is the wave's product, so none shares another's context");
        return agent(proposePrompt(i), {
          schema: proposalSchema,
          cwd: repo,
          label: `propose:${i}`,
        });
      }
    )
  )
).filter(Boolean);

log(`proposals: ${proposals.length}/${waves} workers, ${proposals.reduce((n, p) => n + p.obligations.length, 0)} obligation candidates`);

phase("reduce");

dispatching("reduce", "re-briefed", "the reducer merges every proposer's output on a different tier from any of them; continuing one proposer would let its own axis curate the others");
const reduced = await agent(
  `Merge these decomposition proposals into ONE proposal document for a human-judged landing. Proposals (each has criteria, criteria_why, obligations):
${JSON.stringify(proposals)}

Write the merged document to ${outPath} as markdown:
1. "## Criteria" — the candidate decomposition axes, each with its case, and where proposers disagreed, the disagreement stated plainly (the orchestrator judges criteria before entries).
2. "## Proposed obligations" — deduplicated across proposals (same outcome under different slugs is one entry; keep the sharper wording), each carrying: slug, must_be_true, proof, source, spine_status, fan_out (where proposals disagree on the fan-out, keep the disagreement visible rather than averaging it). Flag conflicts (two proposals wanting incompatible outcomes) explicitly.
3. "## Needs-spine summary" — the entries no exemplar covers, grouped.
4. "## Queue strike candidates" — the queue items proposers matched to proven obligation records, deduplicated, each naming its proven record (the orchestrator deletes struck items from the queue; this workflow never edits it).

Do not write to ${ledgerPath} — the proposal is judged and landed by the orchestrator, never by this workflow. Reply with a three-line summary of the document you wrote.`,
  {
    model: "gpt-5.6-terra",
    effort: "medium",
    cwd: repo,
    label: "reduce",
  }
);

return { proposals: proposals.length, summary: reduced, document: outPath };
