// A deliberately small dogfooding workflow: produce a draft, review that exact
// draft, and deliver only the reviewer's accepted text. The delivery target is
// selected from the server-side grant attached to the saved script.
const draft = await agent(args.writerPrompt, {
  agent: args.writerAgent,
  label: "Write draft",
});

const review = await agent(
  `Review the draft below. Return JSON with {"accepted": boolean, "content": string}.\n\n${draft}`,
  {
    agent: args.reviewerAgent,
    label: "Review draft",
    schema: {
      type: "object",
      required: ["accepted", "content"],
      properties: {
        accepted: { type: "boolean" },
        content: { type: "string" },
      },
      additionalProperties: false,
    },
  },
);

if (!review.accepted) {
  throw new Error("Reviewer rejected the draft");
}

await deliver({ content: review.content }, { target: args.deliveryTarget });
return review.content;
