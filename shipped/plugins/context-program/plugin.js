// Composition policy lives here; token counting, compaction and named
// assemblies are host operations available to any context-granted plugin.
swage.provide.slot("context_program", async (input) => {
  const context = (operation, args = {}) => swage.context({ operation, ...args });
  if (input.assembly_name.trim() !== "" && input.assembly_name.trim() !== "default") {
    return await context("named_assembly");
  }
  const elements = [...input.elements].sort((a, b) => b.priority - a.priority);
  const reports = elements.map((element) => ({ ...element.report }));
  let messages = [];
  let historyRange;
  for (const element of elements) {
    const content = element.rendered_messages;
    if (element.name === "history") {
      historyRange = [messages.length, messages.length + content.length];
    }
    messages.push(...content);
  }
  const compaction = [];
  if (input.budget !== null && await context("count_messages", { messages }) > input.budget && historyRange) {
    const [start, end] = historyRange;
    const nonHistory = [...messages.slice(0, start), ...messages.slice(end)];
    const budget = Math.max(0, input.budget - await context("count_messages", { messages: nonHistory }));
    const compacted = await context("compact_history", { messages: messages.slice(start, end), budget });
    messages = [...messages.slice(0, start), ...compacted.messages, ...messages.slice(end)];
    reports.find((element) => element.name === "history").token_count = compacted.record.tokens_after;
    compaction.push(compacted.record);
  }
  const total = await context("count_messages", { messages });
  return {
    messages,
    report: {
      assembly_name: "default", elements: reports, total_tokens: total,
      budget: input.budget, budget_remaining: input.budget === null ? null : input.budget - total,
      compaction_applied: compaction, warnings: [], errors: [], declared_omissions: [],
      duration: { secs: 0, nanos: 0 },
    },
  };
});
