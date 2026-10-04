/**
 * Who wrote a message, as the person reading the conversation needs it.
 *
 * The provider-facing `role` says how the model was shown a message; the
 * recorded `origin` says who composed it. Only a message the person typed is
 * theirs, so only that one reads as "You" — a schedule's brief or a
 * delegation's notice rides the user role to the model yet is labelled by
 * the platform path that wrote it.
 */
import type { MessageOrigin } from "./protocol/MessageOrigin";
import type { MessageView } from "./types";

type Authored = Pick<MessageView, "role" | "origin">;

/**
 * The message's recorded origin, or the one its role implies for a row
 * appended locally — the person's typed message, the model's streamed reply —
 * before the gateway has recorded it.
 */
export function messageOrigin(message: Authored): MessageOrigin {
  return message.origin ?? (message.role === "user" ? "user" : "assistant");
}

/** Whether the person at this client wrote the message. */
export function isPersonsMessage(message: Authored): boolean {
  return messageOrigin(message) === "user";
}

const AUTHOR_LABELS: Record<MessageOrigin, string> = {
  user: "You",
  assistant: "Assistant",
  tool: "Tool",
  delegation: "Delegation",
  schedule: "Schedule",
  workflow: "Workflow",
  platform: "Swage",
};

/** The author label shown above a message. */
export function authorLabel(message: Authored): string {
  return AUTHOR_LABELS[messageOrigin(message)];
}
