import type { CreateChannelLinkCodeRequest } from "../protocol/CreateChannelLinkCodeRequest";
import * as generated from "../protocol/client";

export async function listChannelStatuses() {
  return (await generated.listChannelStatuses()).channels;
}

export async function listChannelLinks() {
  return (await generated.listChannelLinks()).links;
}

export async function listChannelLinkCodes() {
  return generated.listChannelLinkCodes();
}

export async function createChannelLinkCode(request: CreateChannelLinkCodeRequest) {
  return generated.createChannelLinkCode(request);
}
