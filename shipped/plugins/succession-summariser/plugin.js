// The succession step's summariser: the request the kernel composed — its
// instructions, the history to summarise, and its limits — goes to the model
// under this plugin's grant, and the model's answer is the summary.
swage.provide.slot("succession", async (input) => {
  const reply = await swage.complete({
    messages: input.messages,
    model: input.model,
    max_tokens: input.max_tokens,
    temperature: input.temperature,
  });
  return { summary: reply.content };
});
