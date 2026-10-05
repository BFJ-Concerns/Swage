// Research sweep: from a subject and the purpose the findings must serve, run
// an unattended gather → organise → synthesise pass over live web sources,
// landing everything in the topic's cumulative knowledge base.
//
// Saved-script contract: args = { subject, purpose, maxSources? }. Every
// worker here is a bare dispatch — prompt, tool grants, and the run's
// inherited model — never an agent definition: the roles (scout and
// gatherer, organiser, synthesist) exist only inside this script, so an
// operator's picker never lists them and a same-named saved copy is the
// way to vary them. Each stage's grants are the exact tools it needs, no
// more; none holds config-apply or schedule tools. Knowledge-base writes
// happen inside worker turns, so each is journalled as the agent call
// that made it, and every capture carries that call's scope: a resumed
// sweep re-running an interrupted gather turn re-records under the same
// scope and gets the first attempt's observation back, so no observation
// duplicates even when the live page's bytes changed between attempts.
export const defaults = { maxAttempts: 3 };

const subject = args.subject;
const purpose = args.purpose;
const maxSources = args.maxSources ?? 8;
if (!subject || !purpose) {
  throw new Error("research-sweep needs args.subject and args.purpose");
}

// Standing instructions per role, prefixed to every prompt that role
// receives. The tool lists beside them are the role's whole grant.
const researcherBrief =
  "Research the requested subject from web sources. Record gathered source " +
  "material in the knowledge base and report source failures without " +
  "inventing substitutes.\n\n";
const researcherTools = [
  "web_search",
  "url_fetch",
  "browser_navigate",
  "browser_snapshot",
  "knowledge_record",
];
const organiserBrief =
  "Organise gathered research in the knowledge base. Author orientation " +
  "cards with knowledge_curate_orientation so later work can find its way " +
  "into the corpus, grounding every card in the observation ids it " +
  "signposts; supersede a card when a better one replaces its signpost, " +
  "and retire one whose signpost no longer helps. Curate duplicate or " +
  "superseded observations with knowledge_curate. Never remove source " +
  "material, and never record your own prose as a source observation - " +
  "the knowledge base holds gathered external evidence, and your " +
  "judgements ride cards and curation assertions.\n\n";
const organiserTools = [
  "knowledge_orient",
  "knowledge_search",
  "knowledge_curate",
  "knowledge_curate_orientation",
];
const synthesistBrief =
  "Synthesise findings for the stated purpose using only evidence in the " +
  "knowledge base. Validate the shortest sufficient exact quotation for " +
  "every delivered claim with knowledge_cite, and deliver each claim with " +
  "the durable citation id it returns, the exact quote it accepted, the " +
  "observation id, and the source the search hit names, so a reader can " +
  "follow the claim to its quotation and the quotation to its origin " +
  "without a further lookup.\n\n";
const synthesistTools = ["knowledge_search", "knowledge_cite"];

// Every dispatch records on the run journal whether it continued a held
// worker or re-briefed a fresh one, and why — the choice is this script's,
// made per stage (the shipped set's rule: continue where a later turn
// profits from a worker's accumulated context, re-brief where independence
// is the point or the envelope differs).
const dispatching = (label, mode, reason) => log(`[dispatch] ${label}: ${mode} — ${reason}`);

// Every stage here re-briefs: each role's grant is its whole tool set, and
// a held worker's grants are fixed when it opens, so a scout cannot become
// an organiser or a synthesist without a fresh dispatch — and the
// synthesist's independence from the gathering is the citation contract.

phase("Scope: choose sources");
// The scout's grants exclude knowledge_record; recording waits for the
// gather wave, so a crash here costs nothing.
dispatching("Scope sources", "re-briefed", "the sweep's first worker; its grants (search and fetch, no recording) differ from every later role's, so no later stage can continue it");
const plan = await agent(
  researcherBrief +
    `You are scoping a research sweep.\n` +
    `Subject: ${subject}\n` +
    `Purpose the findings must serve: ${purpose}\n\n` +
    `Choose up to ${maxSources} concrete web sources worth gathering - ` +
    `specific pages or documents, not site homepages. Use web search if you ` +
    `have it; otherwise name canonical sources you know. For each, give the ` +
    `URL and one line on what it should contribute. Prefer primary material ` +
    `over commentary. Record nothing at this stage - you are choosing ` +
    `sources, not gathering them. Return your final answer as JSON only.`,
  {
    tools: researcherTools.filter((tool) => tool !== "knowledge_record"),
    label: "Scope sources",
    schema: {
      type: "object",
      required: ["sources"],
      additionalProperties: false,
      properties: {
        sources: {
          type: "array",
          items: {
            type: "object",
            required: ["url", "why"],
            additionalProperties: false,
            properties: {
              url: { type: "string" },
              why: { type: "string" },
            },
          },
        },
      },
    },
  },
);
// Schema exhaustion returns null; other turn failures still throw. Stop with
// a clear run error before treating an absent plan as a validated object.
if (plan === null) {
  throw new Error("research-sweep could not validate its source plan");
}
// Empty is the one bad shape the schema permits.
if (plan.sources.length === 0) {
  throw new Error("research-sweep could not scope any sources");
}

phase(`Gather: ${plan.sources.length} sources`);
// One pipeline item per source, no cross-item barrier: a source that fails
// to fetch collapses to null and is named in the terminal report rather
// than killing the run. Each worker captures through knowledge_record, so
// the raw body, origin, and extraction land as an immutable observation.
const gathered = await pipeline(
  plan.sources,
  (source) => {
    dispatching(`Gather ${source.url}`, "re-briefed", "one independent gatherer per source: a failed fetch collapses only its own slot, and no source's worker profits from another's context");
    return agent(
      researcherBrief +
        `Gather one source for the research sweep on "${subject}".\n` +
        `Source: ${source.url}\n` +
        `Expected contribution: ${source.why}\n\n` +
        `Fetch it with url_fetch (degrade to the browser only if a plain ` +
        `fetch fails). Record it into the knowledge base with ` +
        `knowledge_record, passing the fetch result's capture reference as ` +
        `fetch_call and nothing else: the platform keeps the bytes it ` +
        `fetched and derives the citable text, so never retype the body or ` +
        `the text yourself. A source you could only read through the ` +
        `browser is recorded with knowledge_record's content (the page's ` +
        `text as the browser showed it) and its url, nothing else; it is ` +
        `searchable but not citable. If the source is unreachable or ` +
        `empty, do not invent a substitute - report the failure with ` +
        `recorded false and observation_id null. Return JSON only.`,
      {
        tools: researcherTools,
        label: `Gather ${source.url}`,
        schema: {
          type: "object",
          required: ["url", "recorded", "observation_id", "note"],
          additionalProperties: false,
          properties: {
            url: { type: "string" },
            recorded: { type: "boolean" },
            observation_id: { type: ["string", "null"] },
            note: { type: "string" },
          },
        },
      },
    );
  },
);

// Source accounting is derived from the knowledge base, never from a
// gatherer's account of its own attempt: a source is gathered when an
// observation for it exists under this run's capture scopes, found by the
// observation id the gatherer reported first and by the plan URL second (a
// page recorded under a redirected or normalised URL is still gathered). A
// source is failed only when no observation exists, and its reason is the
// gatherer's own note or the fact that the worker never replied.
const captured = await capturedSources();
const capturedByUrl = new Map(captured.sources.map((source) => [source.url, source]));
const capturedById = new Map(captured.sources.map((source) => [source.observation_id, source]));
const accounted = plan.sources.map((source, index) => {
  const outcome = gathered[index];
  const byId = outcome && outcome.observation_id ? capturedById.get(outcome.observation_id) : undefined;
  return { source, outcome, landed: byId ?? capturedByUrl.get(source.url) ?? null };
});
const landed = accounted
  .filter((entry) => entry.landed)
  .map((entry) => ({ url: entry.landed.url, observation_id: entry.landed.observation_id }));
const failed = accounted
  .filter((entry) => !entry.landed)
  .map(({ source, outcome }) => ({
    url: source.url,
    reason: outcome === null
      ? "the gather worker failed before replying"
      : outcome.recorded
        ? `the gatherer reported recording ${outcome.observation_id}, but the knowledge base holds no observation for this source: ${outcome.note}`
        : outcome.note,
  }));
log(`gathered ${landed.length}/${plan.sources.length}; ${failed.length} failed`);
if (landed.length === 0) {
  throw new Error("research-sweep gathered nothing; report: " + JSON.stringify(failed));
}

phase("Organise: orientation and curation");
// One organising pass over what this sweep added, in the context of the
// whole corpus: author orientation, assert duplicates/supersessions. The
// organiser judges through its tools; the script hands it ids, not content.
dispatching("Organise corpus", "re-briefed", "the organiser's grants are the curation tools alone, an envelope no gatherer holds; it reads what this sweep recorded through the corpus, not through a gatherer's memory of fetching it");
const organised = await agent(
  organiserBrief +
    `A research sweep on "${subject}" just recorded these observations:\n` +
    landed
      .map((entry) => `- ${entry.observation_id} (${entry.url})`)
      .join("\n") +
    `\n\nOrganise them into the topic's knowledge base. Search the corpus ` +
    `to see what already exists. Author orientation cards with ` +
    `knowledge_curate_orientation - a short signpost titled by what it ` +
    `orients, its content saying what the material covers and where to ` +
    `look, grounded in the observation ids it points to; supersede an ` +
    `existing card only when your new one replaces its signpost. Where a ` +
    `new observation duplicates or supersedes existing material, assert ` +
    `that with knowledge_curate - never delete anything. Observations you ` +
    `judge worth flagging but cannot express as a card or assertion go in ` +
    `your closing note. Zero cards or zero assertions is a valid outcome ` +
    `when nothing warrants them.\n\n` +
    `Return JSON only, exactly this shape:\n` +
    `{"orientation_cards": <count of orientation cards you authored>, ` +
    `"assertions": <count of curation assertions you made>, ` +
    `"note": "one or two sentences on what you did and anything that ` +
    `needs a human eye"}`,
  {
    tools: organiserTools,
    label: "Organise corpus",
    // A live organiser once answered off-schema and exhausted the default
    // budget; the tool work above is already done by then, so extra
    // attempts only re-shape the answer - cheap, and dedup-safe.
    maxAttempts: 5,
    schema: {
      type: "object",
      required: ["orientation_cards", "assertions", "note"],
      additionalProperties: false,
      properties: {
        orientation_cards: { type: "number" },
        assertions: { type: "number" },
        note: { type: "string" },
      },
    },
  },
);

// Schema exhaustion returns null: the organising pass's tool work is done
// by then, but its account is not, and the terminal report promises one.
// Stop rather than complete with an organising pass nobody can read.
if (organised === null) {
  throw new Error("research-sweep could not validate its organising account");
}

phase("Synthesise: findings against the purpose");
// The synthesist reads through search over the accumulated corpus - not this
// sweep's pocket - and validates quotes through cite before reporting them,
// so cross-sweep synthesis is the default shape without bypassing citations.
dispatching("Synthesise findings", "re-briefed", "independence is the point: the synthesist works only from the knowledge base through search and cite, with no memory of gathering, so every claim is traceable to the corpus rather than to a worker's recollection");
const findings = await agent(
  synthesistBrief +
    `Deliver research findings.\n` +
    `Subject: ${subject}\n` +
    `Purpose the findings must serve: ${purpose}\n\n` +
    `Work only from the topic's knowledge base via knowledge_search - no ` +
    `fetching, no memory of this conversation. For every claim, call ` +
    `knowledge_cite with the observation id, extraction version, and ` +
    `shortest sufficient exact quote returned by search. Put the claim in ` +
    `findings only after knowledge_cite accepts the quote, and return with ` +
    `the claim its citation_id, the quote exactly as accepted, the ` +
    `observation_id it was cited from, and the source string the search ` +
    `hit carried (the origin URL or path). If citation validation refuses ` +
    `the quote, the claim does not go in findings - it goes in ` +
    `"withheld", with the reason it could not be supported (nothing in the ` +
    `corpus, evidence conflicts, the evidence is too thin). Never drop an ` +
    `unsupported claim silently. Your findings are about the subject: do ` +
    `not describe what this sweep gathered or failed to gather - the ` +
    `report's source accounting carries that, derived from the knowledge ` +
    `base. Return JSON only.`,
  {
    tools: synthesistTools,
    label: "Synthesise findings",
    schema: {
      type: "object",
      required: ["findings", "withheld"],
      additionalProperties: false,
      properties: {
        findings: {
          type: "array",
          items: {
            type: "object",
            required: ["claim", "citation_id", "quote", "observation_id", "source"],
            additionalProperties: false,
            properties: {
              claim: { type: "string" },
              citation_id: { type: "string" },
              quote: { type: "string" },
              observation_id: { type: "string" },
              source: { type: "string" },
            },
          },
        },
        withheld: {
          type: "array",
          items: {
            type: "object",
            required: ["claim", "reason"],
            additionalProperties: false,
            properties: {
              claim: { type: "string" },
              reason: { type: "string" },
            },
          },
        },
      },
    },
  },
);

// The terminal report names what failed, was omitted, or was withheld -
// the commission's requirement that a sweep never pretends completeness -
// and each finding carries its own trail (quote, observation, source), so
// the reader follows a claim to its evidence without a backend lookup.
// Schema exhaustion returns null, so refuse to present an unvalidated
// synthesis as findings; other agent-call failures still throw.
if (findings === null) {
  throw new Error("research-sweep could not validate its findings");
}
return {
  subject,
  purpose,
  gathered: landed.map((entry) => ({
    url: entry.url,
    observation_id: entry.observation_id,
  })),
  failed_sources: failed,
  organised,
  findings: findings.findings,
  withheld_claims: findings.withheld,
};
