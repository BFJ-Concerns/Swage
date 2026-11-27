import type { PluginSummary } from "../protocol/PluginSummary";
import * as generated from "../protocol/client";

/** The installed plugins with what each provides and where it is enabled. */
export async function listPlugins(): Promise<PluginSummary[]> {
  const response = await generated.listPlugins();
  return response.plugins;
}

/**
 * Install the package at `sourcePath` (a directory on the gateway's host
 * holding `plugin.toml`); an installed name takes a new revision.
 */
export async function installPlugin(sourcePath: string): Promise<PluginSummary> {
  return generated.installPlugin({ source_path: sourcePath });
}

/** Enable or disable a plugin for the install (`topicId` null) or one topic. */
export async function setPluginEnablement(
  name: string,
  topicId: string | null,
  enabled: boolean,
): Promise<PluginSummary> {
  return generated.setPluginEnablement(name, { topic_id: topicId, enabled });
}
