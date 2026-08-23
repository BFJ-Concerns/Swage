// Research sweep: from a topic and the purpose the findings must serve, run
// an unattended gather → organise → synthesise pass over live web sources,
// landing everything in the space's cumulative knowledge base.
//
// Saved-script contract: args = { topic, purpose, maxSources? }. The agents
// named here are agent definitions in the dispatching space: "researcher"
// needs web reach (search where configured, url fetch, browser) plus
// knowledge_record; "organiser" needs knowledge_search, knowledge_curate,
// and knowledge_curate_orientation; "synthesist" needs knowledge_search
// only. None of them holds config-apply or schedule tools.
// Knowledge-base writes happen inside agent turns, so each is journalled as
// the agent call that made it, and every capture carries that call's scope:
// a resumed sweep re-running an interrupted gather turn re-records under
// the same scope and gets the first attempt's observation back, so no
// observation duplicates even when the live page's bytes changed between
// attempts.
export const defaults = { maxAttempts: 3 };

const topic = args.topic;
const purpose = args.purpose;
const maxSources = args.maxSources ?? 8;
if (!topic || !purpose) {
  throw new Error("research-sweep needs args.topic and args.purpose");
}

phase("Scope: choose sources");
// The scout's call-scoped grants exclude knowledge_record; recording waits
// for the gather wave, so a crash here costs nothing.
const plan = await agent(
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
    agent: "researcher",
    tools: [
      "web_search",
      "url_fetch",
      "browser_navigate",
      "browser_snapshot",
      "browser_click",
      "browser_type",
      "browser_select",
      "browser_scroll",
    ],
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
// A schema-failed or turn-failed call throws from agent() itself, so plan is
// a validated object here; empty is the one bad shape the schema permits.
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
      `Gather one source for the research sweep on "${topic}".\n` +
        `Source: ${source.url}\n` +
        `Expected contribution: ${source.why}\n\n` +
        `Fetch it (degrade to the browser only if a plain fetch fails). ` +
        `Record it into the knowledge base with knowledge_record, passing ` +
        `the source URL as the origin, the raw body, and your extracted ` +
        `text. If the source is unreachable or empty, do not invent a ` +
        `substitute - report the failure. Return JSON only.`,
      {
        agent: "researcher",
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

const landed = gathered.filter((entry) => entry && entry.recorded);
const failed = plan.sources
  .map((source, index) => ({ source, outcome: gathered[index] }))
  .filter(({ outcome }) => !outcome || !outcome.recorded)
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
    agent: "organiser",
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

phase("Synthesise: findings against the purpose");
// The synthesist reads only through search over the accumulated corpus -
// not this sweep's pocket - so cross-sweep synthesis is the default shape.
const findings = await agent(
  `Deliver research findings.\n` +
    `Topic: ${topic}\n` +
    `Purpose the findings must serve: ${purpose}\n\n` +
    `Work only from the space's knowledge base via knowledge_search - no ` +
    `fetching, no memory of this conversation. Every claim must be ` +
    `traceable: cite the observation id and quote the supporting text for ` +
    `each - verbatim from the stored extraction, kept to the shortest span ` +
    `that supports the claim. A claim the purpose needs that you cannot ` +
    `pin to an observation does not go in the findings - it goes in ` +
    `"withheld", with the reason it could not be supported (nothing in the ` +
    `corpus, evidence conflicts, the evidence is too thin). Never drop an ` +
    `unsupported claim silently. Return JSON only.`,
  {
    agent: "synthesist",
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
            required: ["claim", "observation_id", "quote"],
            additionalProperties: false,
            properties: {
              claim: { type: "string" },
              observation_id: { type: "string" },
              quote: { type: "string" },
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
// agent() throws on failure, so reaching here means synthesis validated;
// the report still names what failed upstream and what the synthesist
// could not support.
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
