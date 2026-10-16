// The succession step's summariser: the request the kernel composed — its
// instructions and the history to summarise — goes to the model under this
// plugin's grant, and the model's answer is the summary.
swage.provide.slot("succession", async (input) => {
  const reply = await swage.complete({ messages: input.messages, model: input.model });
  return { summary: reply.content };
});
