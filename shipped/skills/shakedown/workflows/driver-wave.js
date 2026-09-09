export const meta = {
  name: "shakedown-driver-wave",
  description:
    "Fan out one charter driver per prepared black-box workspace: each drives its charter at the product's real interface, records evidence into its own workspace, and returns its session report — findings or an explicit null return. Staffing is declared once and overridden per charter; escalation and re-dispatch stay the seat's decisions between waves.",
};

export const defaults = {
  // The driver default is the exploration family's strong mid tier at
  // medium effort (references/charters.md, Staffing and Escalation); which
  // family drives is the campaign staffing table's choice, carried in by
  // the wave input as the model id (the id names the family). The floor
  // tier never drives, and text-only tiers never drive — a driver must see
  // the product's surfaces.
  model: "gpt-5.6-terra",
  effort: "medium",
};

// INPUT (workflow args):
// {
//   "driver": { "model", "effort"? },
//     // the staffing table's driver row, stated once for the wave — the
//     // model id selects the family
//   "charters": [{
//     "id": "charter-slug",              // names the slot in the result
//     "workspace": "/abs/path",          // the charter's PREPARED black-box
//                                        // workspace — this script never
//                                        // prepares one from the repo, and
//                                        // the black-box verification
//                                        // (workspace contents, no source
//                                        // paths, frozen-sha artefacts)
//                                        // is the seat's act before launch
//     "prompt": "…",                     // the charter brief, inline — the
//                                        // sealed script reads no files;
//                                        // the brief carries the journey,
//                                        // oracle cards, conduct and
//                                        // evidence rules
//     "timeoutMs": 7200000,              // per-session bound, sized to the
//                                        // charter's depth — REQUIRED: a
//                                        // charter session must not run
//                                        // unbounded
//     "model"?, "effort"?                // per-charter staffing overrides —
//                                        // tier escalation, misuse charters
//                                        // at high effort
//   }]
// }
//
// Width is the run's, not the script's: the seat sets the product-session
// width with the run's max_concurrent_agent_calls budget at start, from the
// campaign record's measured basis. The wave dispatches every charter and
// lets the engine queue to the declared width. Every workspace must sit
// inside a directory the space has attached — placement never widens the
// run's filesystem authority — and the seat's black-box verification of
// each workspace happens before this run starts: the sealed script cannot
// inspect the filesystem.
//
// OUTPUT: { sessions: [{ id, workspace, output }] } — output is the driver's
// session report text, or null for an operational failure (the slot is
// named, never dropped). A session killed at its timeout with no recorded
// evidence is an invalid observation to re-run, not a null return; rejected
// null returns and escalated re-dispatches are seat decisions between
// waves, never automated here.

const cfg = args || {};
const driver = cfg.driver || {};
if (typeof driver.model !== "string" || !driver.model.trim())
  throw new Error("driver-wave needs driver.model — the staffing table's driver row; the model id names the family");
const charters = Array.isArray(cfg.charters) ? cfg.charters : [];
if (!charters.length) throw new Error("driver-wave needs at least one charter");

const seen = new Set();
for (const c of charters) {
  if (!c || typeof c.id !== "string" || !c.id.trim()) throw new Error("every charter needs an id");
  if (seen.has(c.id)) throw new Error(`duplicate charter id: ${c.id}`);
  seen.add(c.id);
  if (typeof c.workspace !== "string" || !c.workspace.startsWith("/"))
    throw new Error(`charter ${c.id}: workspace must be an absolute path to the prepared black-box workspace`);
  if (!Number.isInteger(c.timeoutMs) || c.timeoutMs <= 0)
    throw new Error(`charter ${c.id}: timeoutMs required — size it to the charter's depth`);
  if (typeof c.prompt !== "string" || !c.prompt.trim()) throw new Error(`charter ${c.id}: needs prompt (the charter brief, inline)`);
  c.resolvedPrompt = c.prompt;
}

phase("drive");

const results = await parallel(
  charters.map((c) => () =>
    agent(
      `${c.resolvedPrompt}

Work the charter from this workspace only (${c.workspace}) — it is your whole world: launch the product from it, drive at the product's real interface, and record evidence as you go into an evidence/ directory inside it (the step record — what you did, what you observed, what you expected; captures of every user-visible surface you touched; the oracle-card comparisons for output you judged). Flag notable captures at capture time — anything that surprised you, resisted you, or seems worth a stronger judge's eyes — with a line on what seemed notable. Diagnosis is not your job: file what you observed, never why you think the product did it. An observation that fits no expected category is still filed. If you end with zero findings, file an explicit null return: what you drove, what you judged, what you attempted to break, with the evidence trail. Reply with your session report: journey coverage as driven, findings (or the null return), flagged captures, and anything you could not exercise.`,
      {
        model: c.model ?? driver.model,
        ...((c.effort ?? driver.effort) ? { effort: c.effort ?? driver.effort } : {}),
        cwd: c.workspace,
        timeoutMs: c.timeoutMs,
        label: `drive:${c.id}`,
        phase: "drive",
        identity: true,
      }
    )
  )
);

// identity:true keeps failed slots named: match results to charters by
// position (parallel preserves order), and surface output null as-is.
return {
  sessions: charters.map((c, i) => ({
    id: c.id,
    workspace: c.workspace,
    output: results[i] ? results[i].output : null,
  })),
};
