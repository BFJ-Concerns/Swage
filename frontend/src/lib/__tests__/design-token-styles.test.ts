/**
 * Regression coverage for the component sweep. These imports exercise the
 * authored component styles, so a future literal reintroduced while retinting
 * one of these surfaces fails alongside the component's behavioural tests.
 */
import { describe, expect, it } from "vitest";

import connectionIndicator from "../components/ConnectionIndicator.svelte?raw";
import conversationList from "../components/ConversationList.svelte?raw";
import messageDisplay from "../components/MessageDisplay.svelte?raw";
import messageInput from "../components/MessageInput.svelte?raw";
import spaceOperations from "../components/SpaceOperations.svelte?raw";
import spaceSelector from "../components/SpaceSelector.svelte?raw";
import toolCallDisplay from "../components/ToolCallDisplay.svelte?raw";
import turnContextInspector from "../components/TurnContextInspector.svelte?raw";
import displayPreferences from "../components/settings/DisplayPreferencesSettingsSurface.svelte?raw";

const RETINTED_COMPONENTS = {
  ConnectionIndicator: connectionIndicator,
  ConversationList: conversationList,
  MessageDisplay: messageDisplay,
  MessageInput: messageInput,
  SpaceOperations: spaceOperations,
  SpaceSelector: spaceSelector,
  ToolCallDisplay: toolCallDisplay,
  TurnContextInspector: turnContextInspector,
  DisplayPreferencesSettingsSurface: displayPreferences,
};

describe("retinted component styles", () => {
  it("keeps the swept components on semantic tokens rather than literal hex or rgba paint", () => {
    for (const [name, source] of Object.entries(RETINTED_COMPONENTS)) {
      expect(source, name).not.toMatch(/(?:#[0-9a-f]{3,8}\b|rgba\s*\()/i);
    }
  });

  it("reserves the error token for the retinted failure surfaces", () => {
    const errorRule = [...connectionIndicator.matchAll(/([^{}]+)\{([^{}]*var\(--error\)[^{}]*)\}/gs)]
      .find(([, selectors]) => selectors.includes(".banner.error"));
    expect(errorRule, "ConnectionIndicator must have an error paint rule").toBeDefined();
    expect(errorRule![1].replace(/\/\*[\s\S]*?\*\//g, "").trim()).toBe(".banner.error");
    expect(toolCallDisplay).toMatch(/\.tool-call\.error\s*\{[^}]*var\(--error\)/s);
    expect(messageDisplay).toMatch(/\.turn-error\s*\{[^}]*var\(--error\)/s);
  });
});
