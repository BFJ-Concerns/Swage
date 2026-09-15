/**
 * Regression coverage for the component sweep. These imports exercise the
 * authored component styles, so a future literal reintroduced while retinting
 * one of these surfaces fails alongside the component's behavioural tests.
 */
import { describe, expect, it } from "vitest";

import appLayout from "../components/AppLayout.svelte?raw";
import connectionIndicator from "../components/ConnectionIndicator.svelte?raw";
import conversationChannel from "../components/ConversationChannel.svelte?raw";
import conversationList from "../components/ConversationList.svelte?raw";
import messageDisplay from "../components/MessageDisplay.svelte?raw";
import messageInput from "../components/MessageInput.svelte?raw";
import operatorDashboard from "../components/OperatorDashboard.svelte?raw";
import operatorHistoryPane from "../components/OperatorHistoryPane.svelte?raw";
import scheduleAutomationPanel from "../components/ScheduleAutomationPanel.svelte?raw";
import topicOperations from "../components/TopicOperations.svelte?raw";
import topicPicker from "../components/TopicPicker.svelte?raw";
import topicSelector from "../components/TopicSelector.svelte?raw";
import toolCallDisplay from "../components/ToolCallDisplay.svelte?raw";
import turnContextInspector from "../components/TurnContextInspector.svelte?raw";
import displayPreferences from "../components/settings/DisplayPreferencesSettingsSurface.svelte?raw";
import codexProviderAuthentication from "../components/settings/CodexProviderAuthentication.svelte?raw";
import providerConfiguration from "../components/settings/ProviderConfiguration.svelte?raw";
import providerCreation from "../components/settings/ProviderCreation.svelte?raw";
import providerDefaultModel from "../components/settings/ProviderDefaultModel.svelte?raw";
import providerModelManagement from "../components/settings/ProviderModelManagement.svelte?raw";

const RETINTED_COMPONENTS = {
  AppLayout: appLayout,
  ConnectionIndicator: connectionIndicator,
  ConversationChannel: conversationChannel,
  ConversationList: conversationList,
  MessageDisplay: messageDisplay,
  MessageInput: messageInput,
  OperatorDashboard: operatorDashboard,
  OperatorHistoryPane: operatorHistoryPane,
  ScheduleAutomationPanel: scheduleAutomationPanel,
  TopicOperations: topicOperations,
  TopicPicker: topicPicker,
  TopicSelector: topicSelector,
  ToolCallDisplay: toolCallDisplay,
  TurnContextInspector: turnContextInspector,
  DisplayPreferencesSettingsSurface: displayPreferences,
  CodexProviderAuthentication: codexProviderAuthentication,
  ProviderConfiguration: providerConfiguration,
  ProviderCreation: providerCreation,
  ProviderDefaultModel: providerDefaultModel,
  ProviderModelManagement: providerModelManagement,
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

  it("keeps schedule outcomes on their semantic roles", () => {
    expect(scheduleAutomationPanel).toMatch(/\.status-active,[\s\S]*?\.status-run-completed_empty\s*\{[^}]*var\(--success\)/);
    expect(scheduleAutomationPanel).toMatch(/\.status-paused\s*\{[^}]*var\(--ink-orange\)/);
    expect(scheduleAutomationPanel).toMatch(/\.status-run-failed\s*\{[^}]*var\(--error\)/);
    expect(scheduleAutomationPanel).toMatch(/\.feedback\.success\s*\{[^}]*var\(--success\)/);
    expect(scheduleAutomationPanel).toMatch(/\.feedback\.error\s*\{[^}]*var\(--error\)/);
  });
});
