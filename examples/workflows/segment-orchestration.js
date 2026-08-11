// Segment orchestration: a long run as fresh-context segments under the
// script as the top surface. Each segment is one lead agent call: the lead
// works, brings its record up to date, sets its wake condition, and ends
// the turn. The next segment starts as a fresh session grounded from run
// state — the brief, the journal the script carries forward, and the
// previous segment's closing report — never the accumulated transcript.
//
// Saved-script contract: args = { brief, maxSegments?, watchTimerMs? }.
// "lead" is an agent definition in the dispatching space with the tools the
// work needs; it may detach children and name them as watches. Each lead
// call is a fresh session by construction (every agent() runs in a new
// ephemeral session), which is the fresh-context property horizon item 7
// names. The wake condition is real: between segments the script sleeps on
// wait(), racing any watched child calls against the journalled timer, and
// the journal records which won.
export const defaults = { agent: "lead", maxAttempts: 2 };

const brief = args.brief;
const maxSegments = args.maxSegments ?? 12;
const watchTimerMs = args.watchTimerMs ?? 15 * 60 * 1000;
if (!brief) {
  throw new Error("segment-orchestration needs args.brief");
}

const segmentSchema = {
  type: "object",
  required: ["closing_report", "next_step", "done", "missing_at_startup", "watch"],
  additionalProperties: false,
  properties: {
    // The successor's inheritance. A thin one starves the next segment,
    // and the prompt says so.
    closing_report: { type: "string" },
    next_step: { type: "string" },
    done: { type: "boolean" },
    // Feeds the grounding bundle: what this segment needed at startup
    // that was not supplied.
    missing_at_startup: { type: "array", items: { type: "string" } },
    // A prompt for one detached helper to run between segments, or null.
    // The script dispatches it and waits on its completion - the
    // completion-watch leg of the wake condition.
    watch: {
      type: ["object", "null"],
      required: ["agent", "prompt"],
      additionalProperties: false,
      properties: {
        agent: { type: "string" },
        prompt: { type: "string" },
      },
    },
  },
};

let report = "First segment: nothing has run yet.";
let wakeNote = "run start";
const segments = [];

for (let segment = 0; segment < maxSegments; segment += 1) {
  phase(`Segment ${segment + 1}`);
  const outcome = await agent(
    `You are the lead of one segment of a long orchestration run. You have ` +
      `a fresh context on purpose: everything you need is below, and what ` +
      `you leave in your closing report is everything your successor gets.\n\n` +
      `## Brief\n${brief}\n\n` +
      `## Previous segment's closing report\n${report}\n\n` +
      `## How this segment woke\n${wakeNote}\n\n` +
      `Work the next step of the brief. Before you finish, bring your ` +
      `record up to date: write a closing report that carries everything a ` +
      `successor with no memory of this segment needs - state, decisions, ` +
      `open questions, exact next step. A thin report starves the next ` +
      `segment. If waiting on something slow would help, put the waiting ` +
      `work in "watch" as a prompt for a detached helper; the run sleeps ` +
      `until the helper completes or the fallback timer lapses. List ` +
      `anything you needed at startup that was not supplied. Set done only ` +
      `when the brief itself is discharged. Return JSON only.`,
    { label: `Segment ${segment + 1}`, schema: segmentSchema },
  );
  // A segment whose reply never validates throws from agent() after its
  // retries, failing the run - the honest outcome, since a successor cannot
  // be grounded from an invalid closing report.
  segments.push({
    segment: segment + 1,
    woke_from: wakeNote,
    missing_at_startup: outcome.missing_at_startup,
  });
  report = outcome.closing_report;
  if (outcome.done) {
    return { done: true, segments, closing_report: report };
  }

  // The segment ends here; the wake condition decides when the next one
  // starts. A declared watch races its completion against the timer; with
  // no watch, the timer alone paces the run. Which one won is in the
  // journal (a completed timer entry means the timer fired) and in the
  // next segment's grounding.
  if (outcome.watch) {
    const helper = agentStart(outcome.watch.prompt, {
      agent: outcome.watch.agent,
      label: `Segment ${segment + 1} watch`,
    });
    const wake = await wait({ watches: [helper], timerMs: watchTimerMs });
    if (wake.wake === "completion") {
      wakeNote = wake.error
        ? `the watched helper failed: ${wake.error.message}`
        : `the watched helper completed: ${JSON.stringify(wake.value)}`;
    } else {
      wakeNote =
        `the ${watchTimerMs}ms fallback timer lapsed before the watched ` +
        `helper finished; its result was not collected`;
    }
  } else {
    await wait({ timerMs: watchTimerMs });
    wakeNote = `the ${watchTimerMs}ms pacing timer lapsed (no watch was set)`;
  }
}

// Segment budget exhausted with the brief still open: return honestly
// rather than pretending completion - the record is current either way.
return { done: false, segments, closing_report: report };
