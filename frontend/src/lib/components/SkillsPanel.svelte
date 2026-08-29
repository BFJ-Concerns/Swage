<script lang="ts">
  import ResolvedSkillsPanel from "./ResolvedSkillsPanel.svelte";

  interface Props {
    spaceId: string | null;
  }

  let { spaceId }: Props = $props();
</script>

<ResolvedSkillsPanel {spaceId} presentation="skills" />
