<script lang="ts">
  interface Props {
    canWrite: boolean;
    isTrusted: boolean;
    disabled?: boolean;
  }

  let { canWrite = $bindable(), isTrusted = $bindable(), disabled = false }: Props = $props();
</script>

<div class="workspace-options">
  <label><input type="checkbox" bind:checked={canWrite} {disabled} /> Allow file changes</label>
  <label><input type="checkbox" bind:checked={isTrusted} {disabled} /> Trust this workspace</label>
</div>

<style>
  .workspace-options { display: flex; flex-wrap: wrap; gap: 12px; }
  label { display: flex; align-items: center; gap: 6px; font-size: 13px; }
</style>
