// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import TopicPicker from "../components/TopicPicker.svelte";
import type { TopicStore } from "../stores/topics";
import type { Topic } from "../types";

type MountedComponent = ReturnType<typeof mount>;

const topic: Topic = {
  id: "topic-1",
  name: "Research",
  slug: "research",
  description: "",
  created_at: "2026-05-26T10:00:00Z",
};

function topicStore(): TopicStore {
  return {
    get topics() { return [topic]; },
    get currentTopic() { return null; },
    get recentTopics() { return []; },
    get isLoading() { return false; },
    loadTopics: async () => {},
    selectTopic: async () => {},
    closeTopic: () => {},
    createTopic: async () => {},
    deleteTopic: async () => {},
  };
}

describe("TopicPicker", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.innerHTML = "";
  });

  it("routes the topic card settings button to topic settings", () => {
    const onOpenTopicSettings = vi.fn();
    component = mount(TopicPicker, {
      target: document.body,
      props: { topicStore: topicStore(), onOpenTopicSettings },
    });

    (document.querySelector(".topic-settings") as HTMLElement).click();

    expect(onOpenTopicSettings).toHaveBeenCalledWith(topic);
  });

  it("offers a start without a topic and reports it", () => {
    const onStartWithoutTopic = vi.fn();
    component = mount(TopicPicker, {
      target: document.body,
      props: { topicStore: topicStore(), onStartWithoutTopic },
    });

    (document.querySelector(".start-general") as HTMLElement).click();

    expect(onStartWithoutTopic).toHaveBeenCalledTimes(1);
  });

  it("renders Open as a real sibling button, not a nested span", () => {
    // UI audit f-2: Open and Settings must be first-class <button> siblings of
    // the card container, never nested interactive widgets inside a button.
    component = mount(TopicPicker, {
      target: document.body,
      props: { topicStore: topicStore() },
    });

    const openBtn = document.querySelector(".open-btn");
    expect(openBtn).toBeInstanceOf(HTMLButtonElement);
    // The card itself is no longer a button — it's a container.
    expect(document.querySelector(".topic-card")).not.toBeInstanceOf(HTMLButtonElement);
    // No nested role="button" spans remain.
    expect(document.querySelectorAll('[role="button"]')).toHaveLength(0);
  });

  it("describes the conversation and workspace-folder grouping topics provide", () => {
    component = mount(TopicPicker, {
      target: document.body,
      props: { topicStore: topicStore() },
    });

    expect(document.body.textContent).toContain("keeps its conversations together");
    expect(document.body.textContent).not.toContain("memory");
    expect(document.body.textContent).toContain("attached workspace folders");
  });
});
