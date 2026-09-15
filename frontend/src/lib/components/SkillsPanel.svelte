<script lang="ts">
  import ResolvedSkillsPanel from "./ResolvedSkillsPanel.svelte";

  interface Props {
    topicId: string | null;
  }

  let { topicId }: Props = $props();
</script>

<ResolvedSkillsPanel {topicId} presentation="skills" />
