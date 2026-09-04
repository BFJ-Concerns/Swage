// Research sweep: from a topic and the purpose the findings must serve, run
// an unattended gather → organise → synthesise pass over live web sources,
// landing everything in the space's cumulative knowledge base.
//
// Saved-script contract: args = { topic, purpose, maxSources? }. Every
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

const topic = args.topic;
const purpose = args.purpose;
const maxSources = args.maxSources ?? 8;
if (!topic || !purpose) {
  throw new Error("research-sweep needs args.topic and args.purpose");
}

// Standing instructions per role, prefixed to every prompt that role
// receives. The tool lists beside them are the role's whole grant.
const researcherBrief =
  "Research the requested topic from web sources. Record gathered source " +
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
  "every delivered claim with knowledge_cite, and report the durable " +
  "citation id it returns.\n\n";
const synthesistTools = ["knowledge_search", "knowledge_cite"];

phase("Scope: choose sources");
// The scout's grants exclude knowledge_record; recording waits for the
// gather wave, so a crash here costs nothing.
const plan = await agent(
  researcherBrief +
    `You are scoping a research sweep.\n` +
    `Topic: ${topic}\n` +
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
  (source) =>
    agent(
      researcherBrief +
        `Gather one source for the research sweep on "${topic}".\n` +
        `Source: ${source.url}\n` +
        `Expected contribution: ${source.why}\n\n` +
        `Fetch it (degrade to the browser only if a plain fetch fails). ` +
        `Record it into the knowledge base with knowledge_record, passing ` +
        `the source URL as the origin, the raw body, and your extracted ` +
        `text. If the source is unreachable or empty, do not invent a ` +
        `substitute - report the failure. Return JSON only.`,
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
    ),
);

const captured = await capturedSources();
const capturedByUrl = new Map(captured.sources.map((source) => [source.url, source]));
const landed = plan.sources
  .map((source, index) =>
    capturedByUrl.get(source.url) ??
    (gathered[index]?.observation_id
      ? { url: source.url, observation_id: gathered[index].observation_id }
      : null),
  )
  .filter(Boolean);
const failed = plan.sources
  .map((source, index) => ({ source, outcome: gathered[index], landed: landed.find((entry) => entry.url === source.url) }))
  .filter(({ landed }) => !landed)
  .map(({ source, outcome }) => ({
    url: source.url,
    reason: outcome ? outcome.note : "gather worker failed",
  }));
log(`gathered ${landed.length}/${plan.sources.length}; ${failed.length} failed`);
if (landed.length === 0) {
  throw new Error("research-sweep gathered nothing; report: " + JSON.stringify(failed));
}

phase("Organise: orientation and curation");
// One organising pass over what this sweep added, in the context of the
// whole corpus: author orientation, assert duplicates/supersessions. The
// organiser judges through its tools; the script hands it ids, not content.
const organised = await agent(
  organiserBrief +
    `A research sweep on "${topic}" just recorded these observations:\n` +
    landed
      .map((entry) => `- ${entry.observation_id} (${entry.url})`)
      .join("\n") +
    `\n\nOrganise them into the space's knowledge base. Search the corpus ` +
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
const findings = await agent(
  synthesistBrief +
    `Deliver research findings.\n` +
    `Topic: ${topic}\n` +
    `Purpose the findings must serve: ${purpose}\n\n` +
    `Work only from the space's knowledge base via knowledge_search - no ` +
    `fetching, no memory of this conversation. For every claim, call ` +
    `knowledge_cite with the observation id, extraction version, and ` +
    `shortest sufficient exact quote returned by search. Put the claim in ` +
    `findings only after knowledge_cite accepts the quote, and return its ` +
    `citation_id with the claim. If citation validation refuses the quote, ` +
    `the claim does not go in findings - it goes in ` +
    `"withheld", with the reason it could not be supported (nothing in the ` +
    `corpus, evidence conflicts, the evidence is too thin). Never drop an ` +
    `unsupported claim silently. Return JSON only.`,
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
            required: ["claim", "citation_id"],
            additionalProperties: false,
            properties: {
              claim: { type: "string" },
              citation_id: { type: "string" },
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
// the commission's requirement that a sweep never pretends completeness.
// Schema exhaustion returns null, so refuse to present an unvalidated
// synthesis as findings; other agent-call failures still throw.
if (findings === null) {
  throw new Error("research-sweep could not validate its findings");
}
return {
  topic,
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
