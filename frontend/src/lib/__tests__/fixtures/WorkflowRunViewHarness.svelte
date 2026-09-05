<script lang="ts">
  import WorkflowRunView from "../../components/WorkflowRunView.svelte";

  let runId = $state("run-a");

  export function show(nextRunId: string): void {
    runId = nextRunId;
  }
</script>

<WorkflowRunView {runId} />
