// Research sweep: from a topic and the purpose the findings must serve, run
// an unattended gather → organise → synthesise pass over live web sources,
// landing everything in the space's cumulative knowledge base.
//
// Saved-script contract: args = { topic, purpose, maxSources? }. The agents
// named here are agent definitions in the dispatching space: "researcher"
// needs web reach (search where configured, url fetch, browser) plus
// knowledge_record; "organiser" needs knowledge_search, knowledge_record,
// and knowledge_curate; "synthesist" needs knowledge_search only. None of
// them holds config-apply or schedule tools. Knowledge-base writes happen
// inside agent turns, so each is journalled as the agent call that made it,
// and the store's hash-dedup means a resumed sweep re-running a gather turn
// cannot duplicate an observation.
export const defaults = { maxAttempts: 3 };

const topic = args.topic;
const purpose = args.purpose;
const maxSources = args.maxSources ?? 8;
if (!topic || !purpose) {
  throw new Error("research-sweep needs args.topic and args.purpose");
}

phase("Scope: choose sources");
// The scout only plans; it records nothing, so a crash here costs nothing.
const plan = await agent(
  `You are scoping a research sweep.\n` +
    `Topic: ${topic}\n` +
    `Purpose the findings must serve: ${purpose}\n\n` +
    `Choose up to ${maxSources} concrete web sources worth gathering - ` +
    `specific pages or documents, not site homepages. Use web search if you ` +
    `have it; otherwise name canonical sources you know. For each, give the ` +
    `URL and one line on what it should contribute. Prefer primary material ` +
    `over commentary. Return your final answer as JSON only.`,
  {
    agent: "researcher",
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
    `to see what already exists. Record orientation notes (as knowledge_record ` +
    `observations of your own authorship) that summarise what this material ` +
    `covers and cite the observation ids they derive from. Where a new ` +
    `observation duplicates or supersedes existing material, assert that ` +
    `with knowledge_curate - never delete anything. Zero notes or zero ` +
    `assertions is a valid outcome when nothing warrants them. In "note", ` +
    `summarise in one or two sentences what you did and anything that ` +
    `needs a human eye. Return JSON only.`,
  {
    agent: "organiser",
    label: "Organise corpus",
    schema: {
      type: "object",
      required: ["orientation_notes", "assertions", "note"],
      additionalProperties: false,
      properties: {
        orientation_notes: { type: "number" },
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
    `that supports the claim. A claim you cannot pin to an observation ` +
    `does not go in the findings. Return JSON only.`,
  {
    agent: "synthesist",
    label: "Synthesise findings",
    schema: {
      type: "object",
      required: ["findings"],
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
      },
    },
  },
);

// The terminal report names what failed or was omitted - the commission's
// requirement that a sweep never pretends completeness.
// agent() throws on failure, so reaching here means synthesis validated;
// the terminal report still names what failed or was omitted upstream.
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
};
