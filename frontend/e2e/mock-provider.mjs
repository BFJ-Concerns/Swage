import { appendFile } from "node:fs/promises";
import { createServer } from "node:http";

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function event(response, type, data) {
  response.write(`event: ${type}\ndata: ${JSON.stringify(data)}\n\n`);
}

async function textResponse(response, chunks, pause = 30) {
  event(response, "message_start", { type: "message_start", message: { usage: { input_tokens: 4, output_tokens: 0 } } });
  event(response, "content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
  for (const text of chunks) {
    event(response, "content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
    await delay(pause);
  }
  event(response, "content_block_stop", { type: "content_block_stop", index: 0 });
  event(response, "message_delta", { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: chunks.length } });
  event(response, "message_stop", { type: "message_stop" });
  response.end();
}

function hasSuccessfulApprovalResult(messages) {
  return messages.some((message) => Array.isArray(message.content)
    && message.content.some((block) => block.type === "tool_result"
      && block.tool_use_id === "e2e-tool-call"
      && block.is_error !== true
      && typeof block.content === "string"
      && block.content.includes("approval-ok")));
}

async function handle(request, response) {
  if (request.method !== "POST" || !request.url?.startsWith("/v1/messages")) {
    response.writeHead(404).end();
    return;
  }
  let raw = "";
  for await (const chunk of request) raw += chunk;
  const body = JSON.parse(raw);
  if (process.env.SWAGE_E2E_PROVIDER_REQUESTS) {
    await appendFile(
      process.env.SWAGE_E2E_PROVIDER_REQUESTS,
      `${JSON.stringify({
        prompt: body.messages.at(-1)?.content ?? null,
        effort: body.output_config?.effort ?? null,
      })}\n`,
    );
  }
  if (JSON.stringify(body.messages).includes("controlled provider failure")) {
    response.writeHead(503, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: { message: "Controlled provider failure" } }));
    return;
  }
  response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", "x-request-id": "e2e-provider" });

  const serialised = JSON.stringify(body.messages);
  if (hasSuccessfulApprovalResult(body.messages)) {
    await textResponse(response, ["Approval ", "completed through the gateway."]);
  } else if (serialised.includes("tool_result")) {
    await textResponse(response, ["Approval did not execute the tool."]);
  } else if (serialised.includes("approve a tool")) {
    event(response, "message_start", { type: "message_start", message: { usage: { input_tokens: 4, output_tokens: 0 } } });
    event(response, "content_block_start", { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "e2e-tool-call", name: "shell", input: {} } });
    event(response, "content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: "{\"command\":\"printf approval-ok\"}" } });
    event(response, "content_block_stop", { type: "content_block_stop", index: 0 });
    event(response, "message_delta", { type: "message_delta", delta: { stop_reason: "tool_use" }, usage: { output_tokens: 1 } });
    event(response, "message_stop", { type: "message_stop" });
    response.end();
  } else if (serialised.includes("cancel this turn")) {
    await textResponse(response, ["Partial response before cancellation. ", "This tail must never render."], 8_000);
  } else if (serialised.includes("reconnect this turn")) {
    await textResponse(response, ["First half before disconnect. ", "Second half after reconnect."], 700);
  } else if (serialised.includes("stream this turn")) {
    await textResponse(response, ["Streamed ", "response reached the browser."], 1_000);
  } else {
    await textResponse(response, ["Streamed ", "response reached the browser."]);
  }
}

createServer((request, response) => void handle(request, response)).listen(
  Number(process.env.SWAGE_E2E_PROVIDER_PORT ?? "33101"),
  "127.0.0.1",
);
