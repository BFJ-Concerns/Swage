<script lang="ts">
  import {
    applyBuilderProposal,
    createBuilderProposal,
    approveWorkspaceExtension,
    createBenchExtension,
    createTeam,
    listBenchExtensions,
    listTeams,
    updateBenchExtension,
    updateTeam,
  } from "../api/builder";
  import { createLens, installLensPreset, updateLens } from "../api/lens";
  import SharedReferenceStudio from "./SharedReferenceStudio.svelte";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { BenchStore } from "../stores/benches";
  import type {
    AppLens,
    BuilderMode,
    BuilderProposalResponse,
    LensDescriptor,
    LensSurface,
    BenchExtensionSummary,
    TeamAgentDefinition,
    TeamSummary,
    TeamTaskDefinition,
  } from "../types";

  type OperatorDashboardModule = typeof import("./OperatorDashboard.svelte");

  interface Props {
    benchStore?: BenchStore;
    desktopRuntime?: DesktopRuntimeInfo | null;
    activeLens: AppLens;
    availableLenses: LensDescriptor[];
    onSelectLens: (next: AppLens) => void | Promise<void>;
    onRefreshLenses: (preferredSurface?: LensSurface) => void | Promise<void>;
    showSharedReferences?: boolean;
    showOperatorDeck?: boolean;
    activeBuilderMode?: BuilderMode;
  }

  let {
    benchStore,
    desktopRuntime,
    activeLens,
    availableLenses,
    onSelectLens,
    onRefreshLenses,
    showSharedReferences = true,
    showOperatorDeck = true,
    activeBuilderMode = "team",
  }: Props = $props();

  let loadError = $state<string | null>(null);
  let isLoading = $state(false);

  let teams = $state<TeamSummary[]>([]);
  let teamError = $state<string | null>(null);
  let teamNotice = $state<string | null>(null);
  let selectedTeamId = $state<string | null>(null);
  let teamName = $state("");
  let teamDescription = $state("");
  let teamEntryAgentId = $state("default");
  let teamAgents = $state<TeamAgentDefinition[]>([]);
  let teamTasks = $state<TeamTaskDefinition[]>([]);
  let selectedAgentDraftId = $state<string | null>(null);
  let agentDraftId = $state("default");
  let agentDraftDisplayName = $state("Default");
  let agentDraftDescription = $state("");
  let agentDraftInstructions = $state("");
  let agentDraftModel = $state("");
  let selectedTaskDraftName = $state<string | null>(null);
  let taskDraftName = $state("");
  let taskDraftDescription = $state("");
  let taskDraftInstructions = $state("");
  let taskDraftModel = $state("");
  let taskDraftOutputSchema = $state("");
  let taskDraftAllowedTools = $state<string[]>([]);

  let lensError = $state<string | null>(null);
  let lensNotice = $state<string | null>(null);
  let selectedLensId = $state<string | null>(null);
  let lensId = $state("");
  let lensTitle = $state("");
  let lensSummary = $state("");
  let lensDefaultSurface = $state<LensSurface>("conversation");
  let lensModulesCsv = $state("conversation, operator");
  let lensProminentTeamsCsv = $state("default");

  let extensions = $state<BenchExtensionSummary[]>([]);
  let extensionError = $state<string | null>(null);
  let extensionNotice = $state<string | null>(null);
  let selectedExtensionName = $state<string | null>(null);
  let extensionName = $state("");
  let extensionDisplayName = $state("");
  let extensionDescription = $state("");
  let extensionToolsCsv = $state("");
  let extensionInstructions = $state(
    "Describe the builder behaviour this extension should inject.",
  );

  let lastLoadedBenchId = "";
  let operatorDashboardModule = $state<Promise<OperatorDashboardModule> | null>(null);
  let builderPrompt = $state(
    "Create a review workspace with an operator-first lens, a delivery team, and a bench brief extension.",
  );
  let builderProposal = $state<BuilderProposalResponse | null>(null);
  let builderRuntimeError = $state<string | null>(null);
  let builderRuntimeNotice = $state<string | null>(null);
  let isGeneratingProposal = $state(false);
  let isApplyingProposal = $state(false);
  let selectedBuilderNode = $state<string>("team:draft");

  const currentBench = $derived(benchStore?.currentBench ?? null);
  const managedExtensions = $derived(
    extensions.filter((extension) => extension.source_kind === "managed"),
  );
  const workspaceCandidates = $derived(
    extensions.filter((extension) => extension.status === "approval_required"),
  );
  const modeTitle = $derived(
    activeBuilderMode.charAt(0).toUpperCase() + activeBuilderMode.slice(1),
  );
  const showsRuntimeBuilder = $derived(["team", "agent", "context"].includes(activeBuilderMode));
  const showsLensBuilder = $derived(activeBuilderMode === "lens");
  const showsTeamBuilder = $derived(activeBuilderMode === "team" || activeBuilderMode === "agent");
  const showsExtensionBuilder = $derived(["team", "skill", "tool"].includes(activeBuilderMode));
  const showsSharedReferenceBuilder = $derived(showSharedReferences && activeBuilderMode === "context");
  const selectedTeam = $derived(
    selectedTeamId
      ? teams.find((team) => team.id === selectedTeamId) ?? null
      : teams[0] ?? null,
  );
  const flowAgents = $derived(teamAgents.length ? teamAgents : selectedTeam?.agents ?? []);
  const flowTasks = $derived(teamTasks.length ? teamTasks : selectedTeam?.tasks ?? []);
  const selectedFlowKind = $derived(selectedBuilderNode.split(":")[0] ?? "team");
  const toolPaletteTarget = $derived(toolTargetLabel());
  const availableToolNames = $derived(discoverToolNames());

  $effect(() => {
    if (!operatorDashboardModule) {
      operatorDashboardModule = import("./OperatorDashboard.svelte");
    }
  });

  $effect(() => {
    const benchId = currentBench?.id ?? "";
    if (!benchId || benchId === lastLoadedBenchId) {
      return;
    }
    lastLoadedBenchId = benchId;
    void refreshBuilderData(benchId);
  });

  function normaliseCsvList(value: string): string[] {
    return value
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);
  }

  function blankAgent(id = "default"): TeamAgentDefinition {
    return {
      id,
      display_name: id === "default" ? "Default" : id,
      description: "",
      instructions: "",
      model: null,
    };
  }

  function blankTask(): TeamTaskDefinition {
    return {
      name: "",
      description: "",
      instructions: "",
      model: null,
      output_schema: null,
      max_output_retries: null,
      tool_allow: null,
      tool_deny: null,
    };
  }

  function resetAgentDraft(agent: TeamAgentDefinition = blankAgent()): void {
    selectedAgentDraftId = agent.id || null;
    agentDraftId = agent.id;
    agentDraftDisplayName = agent.display_name;
    agentDraftDescription = agent.description;
    agentDraftInstructions = agent.instructions;
    agentDraftModel = agent.model ?? "";
  }

  function resetTaskDraft(task: TeamTaskDefinition = blankTask()): void {
    selectedTaskDraftName = task.name || null;
    taskDraftName = task.name;
    taskDraftDescription = task.description;
    taskDraftInstructions = task.instructions;
    taskDraftModel = task.model ?? "";
    taskDraftOutputSchema = task.output_schema
      ? JSON.stringify(task.output_schema, null, 2)
      : "";
    taskDraftAllowedTools = [...(task.tool_allow ?? [])];
  }

  function ensureEntryAgentStillExists(): void {
    if (!teamAgents.some((agent) => agent.id === teamEntryAgentId)) {
      teamEntryAgentId = teamAgents[0]?.id ?? "default";
    }
  }

  function parseOptionalJson(
    raw: string,
    fieldName: string,
  ): Record<string, unknown> | null {
    const trimmed = raw.trim();
    if (!trimmed) return null;

    const parsed = JSON.parse(trimmed) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      throw new Error(`${fieldName} must be a JSON object.`);
    }
    return parsed as Record<string, unknown>;
  }

  function resetTeamForm(): void {
    selectedTeamId = null;
    teamName = "";
    teamDescription = "";
    teamEntryAgentId = "default";
    teamAgents = [blankAgent()];
    teamTasks = [];
    resetAgentDraft(teamAgents[0]);
    resetTaskDraft();
  }

  function resetLensForm(): void {
    selectedLensId = null;
    lensId = "";
    lensTitle = "";
    lensSummary = "";
    lensDefaultSurface = "conversation";
    lensModulesCsv = "conversation, operator";
    lensProminentTeamsCsv = "default";
  }

  function populateLensForm(lens: LensDescriptor): void {
    selectedLensId = lens.id;
    lensId = lens.id;
    lensTitle = lens.title;
    lensSummary = lens.summary;
    lensDefaultSurface = lens.default_surface;
    lensModulesCsv = lens.ui_modules.join(", ");
    lensProminentTeamsCsv = lens.prominent_team_ids.join(", ");
  }

  function populateTeamForm(team: TeamSummary): void {
    selectedTeamId = team.id;
    teamName = team.name;
    teamDescription = team.description;
    teamEntryAgentId = team.entry_agent_id;
    const nextAgents =
      team.agents?.length
        ? [...team.agents]
        : (team.members ?? [team.entry_agent_id]).map((agentId) => blankAgent(agentId));
    teamAgents = nextAgents.length ? nextAgents : [blankAgent(team.entry_agent_id)];
    teamTasks = [...(team.tasks ?? [])];
    ensureEntryAgentStillExists();
    resetAgentDraft(teamAgents[0] ?? blankAgent(team.entry_agent_id));
    resetTaskDraft(teamTasks[0] ?? blankTask());
    selectedBuilderNode = `team:${team.id}`;
  }

  function selectTeamNode(team: TeamSummary | null): void {
    if (!team) return;
    populateTeamForm(team);
    selectedBuilderNode = `team:${team.id}`;
  }

  function selectAgentNode(agent: TeamAgentDefinition): void {
    resetAgentDraft(agent);
    selectedBuilderNode = `agent:${agent.id}`;
  }

  function selectTaskNode(task: TeamTaskDefinition): void {
    resetTaskDraft(task);
    selectedBuilderNode = `task:${task.name}`;
  }

  function saveAgentDraft(): void {
    const id = agentDraftId.trim();
    if (!id) {
      teamError = "Agent ID must not be empty.";
      return;
    }

    const nextAgent: TeamAgentDefinition = {
      id,
      display_name: agentDraftDisplayName.trim() || id,
      description: agentDraftDescription.trim(),
      instructions: agentDraftInstructions.trim(),
      model: agentDraftModel.trim() || null,
    };

    const duplicate = teamAgents.find(
      (agent) => agent.id === id && agent.id !== selectedAgentDraftId,
    );
    if (duplicate) {
      teamError = `Agent ${id} already exists in this team.`;
      return;
    }

    const existingIndex = teamAgents.findIndex(
      (agent) => agent.id === selectedAgentDraftId,
    );
    if (existingIndex >= 0) {
      teamAgents = teamAgents.map((agent, index) =>
        index === existingIndex ? nextAgent : agent,
      );
    } else {
      teamAgents = [...teamAgents, nextAgent];
    }

    if (selectedAgentDraftId === teamEntryAgentId || teamAgents.length === 1) {
      teamEntryAgentId = nextAgent.id;
    }
    selectedAgentDraftId = nextAgent.id;
    ensureEntryAgentStillExists();
    teamError = null;
  }

  function removeSelectedAgent(): void {
    if (!selectedAgentDraftId) return;
    teamAgents = teamAgents.filter((agent) => agent.id !== selectedAgentDraftId);
    if (teamAgents.length === 0) {
      teamAgents = [blankAgent()];
    }
    ensureEntryAgentStillExists();
    resetAgentDraft(teamAgents[0]);
  }

  function saveTaskDraft(): void {
    const name = taskDraftName.trim();
    if (!name) {
      teamError = "Task name must not be empty.";
      return;
    }

    let outputSchema: Record<string, unknown> | null;
    try {
      outputSchema = parseOptionalJson(taskDraftOutputSchema, "Task output schema");
    } catch (error) {
      teamError = error instanceof Error ? error.message : "Task output schema is invalid.";
      return;
    }

    const nextTask: TeamTaskDefinition = {
      name,
      description: taskDraftDescription.trim(),
      instructions: taskDraftInstructions.trim(),
      model: taskDraftModel.trim() || null,
      output_schema: outputSchema,
      max_output_retries: null,
      tool_allow: taskDraftAllowedTools.length ? [...taskDraftAllowedTools] : null,
      tool_deny: null,
    };

    const duplicate = teamTasks.find(
      (task) => task.name === name && task.name !== selectedTaskDraftName,
    );
    if (duplicate) {
      teamError = `Task ${name} already exists in this team.`;
      return;
    }

    const existingIndex = teamTasks.findIndex(
      (task) => task.name === selectedTaskDraftName,
    );
    if (existingIndex >= 0) {
      teamTasks = teamTasks.map((task, index) =>
        index === existingIndex ? nextTask : task,
      );
    } else {
      teamTasks = [...teamTasks, nextTask];
    }

    selectedTaskDraftName = nextTask.name;
    selectedBuilderNode = `task:${nextTask.name}`;
    teamError = null;
  }

  function removeSelectedTask(): void {
    if (!selectedTaskDraftName) return;
    teamTasks = teamTasks.filter((task) => task.name !== selectedTaskDraftName);
    resetTaskDraft(teamTasks[0] ?? blankTask());
  }

  function resetExtensionForm(): void {
    selectedExtensionName = null;
    extensionName = "";
    extensionDisplayName = "";
    extensionDescription = "";
    extensionToolsCsv = "";
    extensionInstructions =
      "Describe the builder behaviour this extension should inject.";
    selectedBuilderNode = "extension:draft";
  }

  function populateExtensionForm(extension: BenchExtensionSummary): void {
    selectedExtensionName = extension.name;
    extensionName = extension.name;
    extensionDisplayName = extension.display_name ?? "";
    extensionDescription = extension.description;
    extensionToolsCsv = extension.requested_tools.join(", ");
    extensionInstructions = extension.instructions;
    selectedBuilderNode = `extension:${extension.name}`;
  }

  function discoverToolNames(): string[] {
    const names = new Set(["shell", "file_read", "file_write", "python_execute"]);
    for (const extension of extensions) {
      for (const tool of extension.requested_tools) {
        if (tool.trim()) names.add(tool.trim());
      }
    }
    for (const task of teamTasks) {
      for (const tool of task.tool_allow ?? []) {
        if (tool.trim()) names.add(tool.trim());
      }
    }
    return [...names].sort();
  }

  function toolTargetLabel(): string {
    if (selectedFlowKind === "task") {
      return selectedTaskDraftName ? `task:${selectedTaskDraftName}` : "task draft";
    }
    if (selectedFlowKind === "extension") {
      return selectedExtensionName ? `extension:${selectedExtensionName}` : "extension draft";
    }
    if (selectedFlowKind === "agent") {
      return selectedAgentDraftId ? `agent:${selectedAgentDraftId}` : "agent draft";
    }
    return selectedTeamId ? `team:${selectedTeamId}` : "team draft";
  }

  function targetHasTool(toolName: string): boolean {
    if (selectedFlowKind === "task") {
      return taskDraftAllowedTools.includes(toolName);
    }
    if (selectedFlowKind === "extension") {
      return normaliseCsvList(extensionToolsCsv).includes(toolName);
    }
    return false;
  }

  function toggleToolForSelectedNode(toolName: string): void {
    if (selectedFlowKind === "task") {
      taskDraftAllowedTools = taskDraftAllowedTools.includes(toolName)
        ? taskDraftAllowedTools.filter((tool) => tool !== toolName)
        : [...taskDraftAllowedTools, toolName].sort();
      return;
    }

    if (selectedFlowKind === "extension") {
      const tools = normaliseCsvList(extensionToolsCsv);
      extensionToolsCsv = (tools.includes(toolName)
        ? tools.filter((tool) => tool !== toolName)
        : [...tools, toolName].sort()
      ).join(", ");
    }
  }

  function canAttachToolsToSelectedNode(): boolean {
    return selectedFlowKind === "task" || selectedFlowKind === "extension";
  }

  resetTeamForm();

  async function refreshBuilderData(benchId: string): Promise<void> {
    isLoading = true;
    loadError = null;
    try {
      const [nextTeams, nextExtensions] = await Promise.all([
        listTeams(benchId),
        listBenchExtensions(benchId),
      ]);
      teams = nextTeams;
      extensions = nextExtensions;
    } catch (error) {
      loadError =
        error instanceof Error ? error.message : "Failed to load builder authoring data.";
    } finally {
      isLoading = false;
    }
  }

  async function saveTeam(): Promise<void> {
    if (!currentBench) return;
    teamError = null;
    teamNotice = null;

    try {
      const payload = {
        name: teamName,
        description: teamDescription,
        entry_agent_id: teamEntryAgentId,
        agents: teamAgents,
        tasks: teamTasks,
      };

      const saved = selectedTeamId
        ? await updateTeam(selectedTeamId, payload)
        : await createTeam(currentBench.id, payload);

      teamNotice = selectedTeamId
        ? `Updated team ${saved.name}.`
        : `Created team ${saved.name}.`;
      await refreshBuilderData(currentBench.id);
      populateTeamForm(saved);
    } catch (error) {
      teamError = error instanceof Error ? error.message : "Failed to save team.";
    }
  }

  async function saveLens(): Promise<void> {
    lensError = null;
    lensNotice = null;

    try {
      const payload = {
        title: lensTitle,
        summary: lensSummary,
        default_surface: lensDefaultSurface,
        ui_modules: normaliseCsvList(lensModulesCsv) as LensSurface[],
        prominent_team_ids: normaliseCsvList(lensProminentTeamsCsv),
      };

      const saved = selectedLensId
        ? await updateLens(selectedLensId, payload)
        : await createLens({
            id: lensId,
            ...payload,
          });

      lensNotice = selectedLensId
        ? `Updated lens ${saved.title}.`
        : `Created lens ${saved.title}.`;
      await onRefreshLenses(saved.default_surface);
      populateLensForm(saved);
    } catch (error) {
      lensError = error instanceof Error ? error.message : "Failed to save lens.";
    }
  }

  async function installResearchLens(): Promise<void> {
    lensError = null;
    lensNotice = null;

    try {
      const installed = await installLensPreset("research");
      lensNotice = `Installed preset lens ${installed.title}.`;
      await onRefreshLenses(installed.default_surface);
      populateLensForm(installed);
    } catch (error) {
      lensError =
        error instanceof Error ? error.message : "Failed to install preset lens.";
    }
  }

  async function saveExtension(): Promise<void> {
    if (!currentBench) return;
    extensionError = null;
    extensionNotice = null;

    try {
      const payload = {
        display_name: extensionDisplayName || null,
        description: extensionDescription,
        requested_tools: normaliseCsvList(extensionToolsCsv),
        instructions: extensionInstructions,
      };

      const saved = selectedExtensionName
        ? await updateBenchExtension(currentBench.id, selectedExtensionName, payload)
        : await createBenchExtension(currentBench.id, {
            name: extensionName,
            ...payload,
          });

      extensionNotice = selectedExtensionName
        ? `Updated extension ${saved.name}.`
        : `Created extension ${saved.name}.`;
      await refreshBuilderData(currentBench.id);
      populateExtensionForm(saved);
    } catch (error) {
      extensionError =
        error instanceof Error ? error.message : "Failed to save extension.";
    }
  }

  async function importWorkspaceExtension(name: string): Promise<void> {
    if (!currentBench) return;
    extensionError = null;
    extensionNotice = null;

    try {
      const imported = await approveWorkspaceExtension(currentBench.id, name);
      extensionNotice = `Approved workspace extension ${imported.name}.`;
      await refreshBuilderData(currentBench.id);
      populateExtensionForm(imported);
    } catch (error) {
      extensionError =
        error instanceof Error ? error.message : "Failed to approve extension.";
    }
  }

  async function handleTeamTopologyUpdate(): Promise<void> {
    if (!currentBench) return;
    await refreshBuilderData(currentBench.id);
  }

  async function requestBuilderProposal(): Promise<void> {
    if (!currentBench) return;
    builderRuntimeError = null;
    builderRuntimeNotice = null;
    isGeneratingProposal = true;

    try {
      builderProposal = await createBuilderProposal(currentBench.id, builderPrompt);
      builderRuntimeNotice = builderProposal.can_apply
        ? "Reviewed proposal ready."
        : "Proposal returned diagnostics that need attention before apply.";
    } catch (error) {
      builderRuntimeError =
        error instanceof Error ? error.message : "Failed to generate builder proposal.";
    } finally {
      isGeneratingProposal = false;
    }
  }

  async function applyCurrentProposal(): Promise<void> {
    if (!currentBench || !builderProposal) return;
    builderRuntimeError = null;
    builderRuntimeNotice = null;
    isApplyingProposal = true;

    try {
      const response = await applyBuilderProposal(
        currentBench.id,
        builderProposal.operations.map((operation) => operation.operation),
        builderPrompt,
      );

      if (!response.body.success) {
        builderRuntimeError =
          response.body.diagnostics.join(" ") || "Builder apply failed review.";
        return;
      }

      builderRuntimeNotice = response.body.summary;
      await Promise.all([
        refreshBuilderData(currentBench.id),
        onRefreshLenses(),
      ]);
    } catch (error) {
      builderRuntimeError =
        error instanceof Error ? error.message : "Failed to apply builder proposal.";
    } finally {
      isApplyingProposal = false;
    }
  }
</script>

<section class="builder-deck">
  <header class="builder-hero">
    <div class="hero-copy">
      <p class="eyebrow">Builder Lens</p>
      <h1>Author the workspace, then let the agent live in it.</h1>
      <p class="lede">
        Builder mode now edits the durable shape of a bench: which team leads
        it, which extension bundles are trusted, and which presentation lens
        should take over next.
      </p>
    </div>

    <div class="status-board">
      <article class="status-card">
        <span class="label">Current bench</span>
        <strong>{currentBench?.name ?? "No bench selected"}</strong>
        <span class="meta mono">
          {currentBench ? `bench:${currentBench.slug}` : "scratch-only until selected"}
        </span>
      </article>

      <article class="status-card">
        <span class="label">Runtime</span>
        <strong>{desktopRuntime ? "Desktop shell" : "Direct web"}</strong>
        <span class="meta">
          {desktopRuntime?.managedGateway ? "Managed gateway" : "Gateway-served UI"}
        </span>
      </article>

      <article class="status-card">
        <span class="label">Builder mode</span>
        <strong>{modeTitle}</strong>
        <span class="meta">
          {managedExtensions.length} managed extension(s) · {workspaceCandidates.length} workspace candidate(s)
        </span>
      </article>
    </div>
  </header>

  {#if !currentBench}
    <section class="empty-state">
      Select a bench before editing teams or extensions.
    </section>
  {:else}
    <section class="builder-module-row" aria-label="Builder modules">
      <article class="builder-module flow-module">
        <header class="module-head">
          <h3>Orchestration flow</h3>
          <span class="chip subtle">{teams.length} team(s)</span>
        </header>
        <div class="flow-board" role="list" aria-label="Team topology">
          {#each teams as team (team.id)}
            <button
              type="button"
              class="flow-node team-node"
              class:active={selectedBuilderNode === `team:${team.id}`}
              onclick={() => selectTeamNode(team)}
            >
              <span>Team</span>
              <strong>{team.name}</strong>
              <small>{team.agents?.length ?? team.members.length} agent(s)</small>
            </button>
          {:else}
            <button
              type="button"
              class="flow-node team-node"
              class:active={selectedBuilderNode === "team:draft"}
              onclick={() => {
                resetTeamForm();
                selectedBuilderNode = "team:draft";
              }}
            >
              <span>Team</span>
              <strong>Draft team</strong>
              <small>not saved yet</small>
            </button>
          {/each}

          {#each flowAgents as agent (agent.id)}
            <button
              type="button"
              class="flow-node agent-node"
              class:active={selectedBuilderNode === `agent:${agent.id}`}
              onclick={() => selectAgentNode(agent)}
            >
              <span>Agent</span>
              <strong>{agent.display_name || agent.id}</strong>
              <small>{agent.model ?? "inherited model"}</small>
            </button>
          {/each}

          {#each flowTasks as task (task.name)}
            <button
              type="button"
              class="flow-node task-node"
              class:active={selectedBuilderNode === `task:${task.name}`}
              onclick={() => selectTaskNode(task)}
            >
              <span>Task</span>
              <strong>{task.name}</strong>
              <small>{(task.tool_allow ?? []).join(", ") || "no tool allow-list"}</small>
            </button>
          {/each}
        </div>
      </article>

      <article class="builder-module inspector-module">
        <header class="module-head">
          <h3>Agent inspector</h3>
          <span class="chip subtle">{toolPaletteTarget}</span>
        </header>
        {#if selectedFlowKind === "team"}
          <dl class="inspector-list">
            <div>
              <dt>Team</dt>
              <dd>{teamName || selectedTeam?.name || "Draft"}</dd>
            </div>
            <div>
              <dt>Entry agent</dt>
              <dd>{teamEntryAgentId}</dd>
            </div>
            <div>
              <dt>Description</dt>
              <dd>{teamDescription || selectedTeam?.description || "No description"}</dd>
            </div>
          </dl>
        {:else if selectedFlowKind === "agent"}
          <dl class="inspector-list">
            <div>
              <dt>Agent</dt>
              <dd>{agentDraftDisplayName || agentDraftId}</dd>
            </div>
            <div>
              <dt>Model</dt>
              <dd>{agentDraftModel || "Inherited"}</dd>
            </div>
            <div>
              <dt>Instructions</dt>
              <dd>{agentDraftInstructions || "No instructions yet"}</dd>
            </div>
          </dl>
        {:else if selectedFlowKind === "task"}
          <dl class="inspector-list">
            <div>
              <dt>Task</dt>
              <dd>{taskDraftName || "Draft task"}</dd>
            </div>
            <div>
              <dt>Model</dt>
              <dd>{taskDraftModel || "Inherited"}</dd>
            </div>
            <div>
              <dt>Allowed tools</dt>
              <dd>{taskDraftAllowedTools.join(", ") || "None"}</dd>
            </div>
          </dl>
        {:else}
          <dl class="inspector-list">
            <div>
              <dt>Extension</dt>
              <dd>{extensionDisplayName || selectedExtensionName || "Draft extension"}</dd>
            </div>
            <div>
              <dt>Requested tools</dt>
              <dd>{normaliseCsvList(extensionToolsCsv).join(", ") || "None"}</dd>
            </div>
            <div>
              <dt>Status</dt>
              <dd>{extensions.find((extension) => extension.name === selectedExtensionName)?.status ?? "Draft"}</dd>
            </div>
          </dl>
        {/if}
      </article>

      <article class="builder-module tool-palette-module">
        <header class="module-head">
          <h3>Tool palette</h3>
          <span class="chip subtle">{availableToolNames.length} tool(s)</span>
        </header>
        <div class="tool-palette" aria-label="Tool palette">
          {#each availableToolNames as toolName}
            <button
              type="button"
              class="tool-chip"
              class:active={targetHasTool(toolName)}
              disabled={!canAttachToolsToSelectedNode()}
              onclick={() => toggleToolForSelectedNode(toolName)}
            >
              {toolName}
            </button>
          {/each}
        </div>
        {#if !canAttachToolsToSelectedNode()}
          <p class="muted-note">
            Select a task or extension node to attach tools. Agents inherit
            tools through task allow-lists and extension bundles.
          </p>
        {/if}
      </article>
    </section>

    <section class="builder-grid">
      {#if showsRuntimeBuilder}
        <article class="builder-card conversational-card">
        <div class="card-heading">
          <div>
            <p class="section-kicker">Conversation</p>
            <h2>Conversational builder runtime</h2>
          </div>
          <span class="chip">{builderProposal?.operations.length ?? 0} change(s)</span>
        </div>
        <p class="section-copy">
          Describe the workspace you want in natural language. The builder
          runtime turns that request into a reviewed change set, then applies it
          through the same durable config path the visual editors use.
        </p>
        {#if builderRuntimeError}
          <p class="feedback error">{builderRuntimeError}</p>
        {/if}
        {#if builderRuntimeNotice}
          <p class="feedback success">{builderRuntimeNotice}</p>
        {/if}
        <label>
          <span>Workspace request</span>
          <textarea bind:value={builderPrompt} rows="5"></textarea>
        </label>
        <div class="inline-actions">
          <button class="primary" onclick={requestBuilderProposal} disabled={isGeneratingProposal}>
            {isGeneratingProposal ? "Reviewing request…" : "Propose changes"}
          </button>
          <button
            onclick={applyCurrentProposal}
            disabled={!builderProposal || !builderProposal.can_apply || isApplyingProposal}
          >
            {isApplyingProposal ? "Applying…" : "Apply reviewed changes"}
          </button>
        </div>

        {#if builderProposal}
          <div class="proposal-stack">
            <div class="proposal-summary">
              <strong>{builderProposal.summary}</strong>
              {#if builderProposal.diagnostics.length}
                <ul class="diagnostic-list">
                  {#each builderProposal.diagnostics as diagnostic}
                    <li>{diagnostic}</li>
                  {/each}
                </ul>
              {/if}
            </div>

            <div class="item-list">
              {#each builderProposal.operations as reviewed}
                <div class="list-item candidate reviewed-operation" class:blocked={!reviewed.ready}>
                  <div>
                    <strong>{reviewed.summary}</strong>
                    <span>{reviewed.kind}</span>
                    {#if reviewed.diagnostics.length}
                      <ul class="diagnostic-list compact">
                        {#each reviewed.diagnostics as diagnostic}
                          <li>{diagnostic}</li>
                        {/each}
                      </ul>
                    {/if}
                  </div>
                  <span class:chip={true} class:subtle={!reviewed.ready}>
                    {reviewed.ready ? "Ready" : "Blocked"}
                  </span>
                </div>
              {/each}
            </div>
          </div>
        {/if}
        </article>
      {/if}

      {#if showsLensBuilder}
        <article class="builder-card">
        <div class="card-heading">
          <div>
            <p class="section-kicker">Presentation</p>
            <h2>Lens catalogue</h2>
          </div>
          <span class="chip">{activeLens}</span>
        </div>
        <p class="section-copy">
          Lenses are now durable records. Create authored lenses, install a
          premade research lens, and choose which UI modules and team focus the
          runtime should foreground.
        </p>
        {#if lensError}
          <p class="feedback error">{lensError}</p>
        {/if}
        {#if lensNotice}
          <p class="feedback success">{lensNotice}</p>
        {/if}
        <label>
          <span>Lens ID</span>
          <input bind:value={lensId} placeholder="research" disabled={!!selectedLensId} />
        </label>
        <label>
          <span>Title</span>
          <input bind:value={lensTitle} placeholder="Research lens" />
        </label>
        <label>
          <span>Summary</span>
          <textarea bind:value={lensSummary} rows="3"></textarea>
        </label>
        <label>
          <span>UI modules</span>
          <input bind:value={lensModulesCsv} placeholder="conversation, operator" />
        </label>
        <label>
          <span>Default surface</span>
          <select bind:value={lensDefaultSurface}>
            <option value="conversation">Conversation</option>
            <option value="operator">Operator</option>
            <option value="builder">Builder deck</option>
          </select>
        </label>
        <label>
          <span>Prominent teams</span>
          <input bind:value={lensProminentTeamsCsv} placeholder="research-lead, reviewer" />
        </label>
        <div class="inline-actions">
          <button class="primary" onclick={saveLens}>
            {selectedLensId ? "Update lens" : "Create lens"}
          </button>
          <button onclick={resetLensForm}>Reset</button>
          <button onclick={installResearchLens}>Install research preset</button>
        </div>
        <div class="item-list">
          {#each availableLenses as lens}
            <div class="list-item candidate">
              <button class="lens-list-button" onclick={() => populateLensForm(lens)}>
                <strong>{lens.title}</strong>
                <span>{lens.ui_modules.join(", ")}</span>
              </button>
              <div class="lens-actions">
                <span class="chip subtle">{lens.source_kind}</span>
                <button
                  class:activeChoice={activeLens === lens.id}
                  onclick={() => onSelectLens(lens.id)}
                >
                  {activeLens === lens.id ? "Active lens" : "Activate"}
                </button>
              </div>
            </div>
          {/each}
        </div>
        </article>
      {/if}

      {#if showsTeamBuilder}
        <article class="builder-card">
        <div class="card-heading">
          <div>
            <p class="section-kicker">Team</p>
            <h2>Entry team</h2>
          </div>
          {#if isLoading}
            <span class="chip">Loading…</span>
          {/if}
        </div>
        <p class="section-copy">
          Shape the durable team that a bench exposes for topology runs and
          builder-side orchestration.
        </p>
        {#if teamError}
          <p class="feedback error">{teamError}</p>
        {/if}
        {#if teamNotice}
          <p class="feedback success">{teamNotice}</p>
        {/if}
        <label>
          <span>Team name</span>
          <input bind:value={teamName} placeholder="workspace-core" />
        </label>
        <label>
          <span>Description</span>
          <textarea bind:value={teamDescription} rows="3"></textarea>
        </label>
        <label>
          <span>Entry agent</span>
          <select bind:value={teamEntryAgentId}>
            {#each teamAgents as agent}
              <option value={agent.id}>{agent.display_name || agent.id}</option>
            {/each}
          </select>
        </label>
        <div class="authoring-columns">
          <section class="authoring-panel">
            <div class="subheading">
              <strong>Agents</strong>
              <span>{teamAgents.length} defined</span>
            </div>
            <div class="item-list compact-list">
              {#each teamAgents as agent}
                <button class="list-item" onclick={() => resetAgentDraft(agent)}>
                  <strong>{agent.display_name || agent.id}</strong>
                  <span>{agent.model ?? "Inherited model"}</span>
                </button>
              {/each}
            </div>
            <label>
              <span>Agent ID</span>
              <input bind:value={agentDraftId} placeholder="reviewer" />
            </label>
            <label>
              <span>Display name</span>
              <input bind:value={agentDraftDisplayName} placeholder="Reviewer" />
            </label>
            <label>
              <span>Description</span>
              <textarea bind:value={agentDraftDescription} rows="2"></textarea>
            </label>
            <label>
              <span>Instructions</span>
              <textarea bind:value={agentDraftInstructions} rows="4"></textarea>
            </label>
            <label>
              <span>Model override</span>
              <input bind:value={agentDraftModel} placeholder="claude-sonnet-4-20250514" />
            </label>
            <div class="inline-actions">
              <button onclick={saveAgentDraft}>
                {selectedAgentDraftId ? "Update agent draft" : "Add agent"}
              </button>
              <button onclick={() => resetAgentDraft(blankAgent())}>New draft</button>
              <button onclick={removeSelectedAgent} disabled={teamAgents.length <= 1}>
                Remove
              </button>
            </div>
          </section>

          <section class="authoring-panel">
            <div class="subheading">
              <strong>Tasks</strong>
              <span>{teamTasks.length} defined</span>
            </div>
            <div class="item-list compact-list">
              {#each teamTasks as task}
                <button class="list-item" onclick={() => resetTaskDraft(task)}>
                  <strong>{task.name}</strong>
                  <span>{task.model ?? "Inherited model"}</span>
                </button>
              {/each}
            </div>
            <label>
              <span>Task name</span>
              <input bind:value={taskDraftName} placeholder="review-plan" />
            </label>
            <label>
              <span>Description</span>
              <textarea bind:value={taskDraftDescription} rows="2"></textarea>
            </label>
            <label>
              <span>Instructions</span>
              <textarea bind:value={taskDraftInstructions} rows="4"></textarea>
            </label>
            <label>
              <span>Model override</span>
              <input bind:value={taskDraftModel} placeholder="claude-haiku-4" />
            </label>
            <label>
              <span>Output schema JSON</span>
              <textarea bind:value={taskDraftOutputSchema} rows="5"></textarea>
            </label>
            <div class="inline-actions">
              <button onclick={saveTaskDraft}>
                {selectedTaskDraftName ? "Update task draft" : "Add task"}
              </button>
              <button onclick={() => resetTaskDraft(blankTask())}>New draft</button>
              <button onclick={removeSelectedTask} disabled={!selectedTaskDraftName}>
                Remove
              </button>
            </div>
          </section>
        </div>
        <div class="inline-actions">
          <button class="primary" onclick={saveTeam}>
            {selectedTeamId ? "Update team" : "Create team"}
          </button>
          <button onclick={resetTeamForm}>Reset</button>
        </div>
        <div class="item-list">
          {#each teams as team}
            <button class="list-item" onclick={() => populateTeamForm(team)}>
              <strong>{team.name}</strong>
              <span>{team.agents?.length ?? team.members.length} agent(s) · {team.tasks?.length ?? 0} task(s)</span>
            </button>
          {/each}
        </div>
        </article>
      {/if}

      {#if showsExtensionBuilder}
        <article class="builder-card extension-card">
        <div class="card-heading">
          <div>
            <p class="section-kicker">Extension</p>
            <h2>Bench extension bundles</h2>
          </div>
          <span class="chip">{managedExtensions.length} active</span>
        </div>
        <p class="section-copy">
          Managed extensions are written into the bench config tree and load
          again after restart. Workspace bundles stay inert until the builder
          approves them.
        </p>
        {#if loadError}
          <p class="feedback error">{loadError}</p>
        {/if}
        {#if extensionError}
          <p class="feedback error">{extensionError}</p>
        {/if}
        {#if extensionNotice}
          <p class="feedback success">{extensionNotice}</p>
        {/if}
        <label>
          <span>Canonical name</span>
          <input bind:value={extensionName} placeholder="builder-guardrails" />
        </label>
        <label>
          <span>Display name</span>
          <input bind:value={extensionDisplayName} placeholder="Builder Guardrails" />
        </label>
        <label>
          <span>Description</span>
          <textarea bind:value={extensionDescription} rows="3"></textarea>
        </label>
        <label>
          <span>Requested tools</span>
          <input bind:value={extensionToolsCsv} placeholder="shell, file_read" />
        </label>
        <label>
          <span>Instructions</span>
          <textarea bind:value={extensionInstructions} rows="8"></textarea>
        </label>
        <div class="inline-actions">
          <button class="primary" onclick={saveExtension}>
            {selectedExtensionName ? "Update extension" : "Create extension"}
          </button>
          <button onclick={resetExtensionForm}>Reset</button>
        </div>
        <div class="stack">
          <div>
            <h3>Managed extensions</h3>
            <div class="item-list">
              {#each managedExtensions as extension}
                <button class="list-item" onclick={() => populateExtensionForm(extension)}>
                  <strong>{extension.display_name ?? extension.name}</strong>
                  <span>{extension.requested_tools.join(", ") || "No requested tools"}</span>
                </button>
              {/each}
            </div>
          </div>
          <div>
            <h3>Workspace candidates</h3>
            <div class="item-list">
              {#each workspaceCandidates as extension}
                <div class="list-item candidate">
                  <div>
                    <strong>{extension.display_name ?? extension.name}</strong>
                    <span>{extension.description}</span>
                  </div>
                  <button class="approve-btn" onclick={() => importWorkspaceExtension(extension.name)}>
                    Approve import
                  </button>
                </div>
              {/each}
              {#if workspaceCandidates.length === 0}
                <p class="muted-note">
                  No workspace extension candidates found under
                  <code>.swage/extensions</code>.
                </p>
              {/if}
            </div>
          </div>
        </div>
        </article>
      {/if}

      {#if showsSharedReferenceBuilder}
        <SharedReferenceStudio
          benchId={currentBench.id}
          onTeamTopologyUpdated={handleTeamTopologyUpdate}
        />
      {/if}
    </section>

    {#if showOperatorDeck}
      <div class="operator-shell">
        {#if operatorDashboardModule}
          {#await operatorDashboardModule then module}
            {@const OperatorDashboard = module.default}
            <OperatorDashboard />
          {:catch error}
            <div class="operator-shell-state" role="alert">
              {error instanceof Error
                ? error.message
                : "Failed to load the operator deck."}
            </div>
          {/await}
        {:else}
          <div class="operator-shell-state">Loading operator deck…</div>
        {/if}
      </div>
    {/if}
  {/if}
</section>

<style>
  /*
    Riso/cyanotype builder deck. Flat ink-bordered panels, mono kickers,
    Bebas Neue display headings, fluoro accent for primary actions and
    hot states. No rounded corners, no soft shadows.
  */

  .builder-deck {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 22px;
    min-height: 0;
    overflow: auto;
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
  }

  .builder-hero {
    display: grid;
    grid-template-columns: minmax(0, 1.25fr) minmax(320px, 0.95fr);
    gap: 16px;
    align-items: stretch;
  }

  .hero-copy,
  .status-board,
  .builder-card {
    border: 2px solid var(--ink);
    background: var(--paper);
  }

  .hero-copy {
    padding: 24px 26px;
  }

  .eyebrow,
  .section-kicker,
  .label {
    margin: 0 0 6px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .section-kicker {
    margin-bottom: 4px;
  }

  .label {
    color: var(--ink);
  }

  h1 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: clamp(2rem, 3vw, 2.6rem);
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink);
  }

  h2,
  h3 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    line-height: 1.05;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink);
  }

  h2 { font-size: 18px; }
  h3 { font-size: 16px; }

  .lede,
  .section-copy {
    margin: 12px 0 0;
    max-width: 50ch;
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--ink-soft);
  }

  .status-board {
    display: grid;
    gap: 0;
    padding: 16px 18px;
  }

  .status-card,
  .list-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px 0;
    border-bottom: 1px dotted var(--rule-soft);
    background: transparent;
  }

  .status-card:last-child,
  .list-item:last-child {
    border-bottom: none;
  }

  strong {
    font-size: 14px;
    color: var(--ink);
    font-weight: 600;
  }

  .meta,
  .muted-note,
  .list-item span {
    color: var(--ink-soft);
    font-size: 12px;
    line-height: 1.5;
  }

  .mono,
  code {
    font-family: var(--font-mono);
    font-size: 11.5px;
    letter-spacing: 0.04em;
  }

  .empty-state {
    padding: 22px;
    border: 2px dashed var(--rule-soft);
    color: var(--ink-soft);
    background: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .builder-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 16px;
  }

  .builder-module-row {
    display: grid;
    grid-template-columns: minmax(320px, 1.35fr) minmax(240px, 0.85fr) minmax(240px, 0.8fr);
    gap: 12px;
  }

  .builder-module {
    border: 2px solid var(--ink);
    background: var(--paper);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }

  .module-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    border-bottom: 1px solid var(--ink);
    padding-bottom: 6px;
  }

  .builder-module h3 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 16px;
    line-height: 1.05;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .flow-board {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
  }

  .flow-node {
    min-height: 92px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: flex-start;
    justify-content: space-between;
    padding: 10px;
    border: 1px solid var(--ink);
    background: rgba(14, 59, 102, 0.04);
    color: var(--ink);
    text-align: left;
    cursor: pointer;
  }

  .flow-node:hover,
  .flow-node.active {
    background: var(--ink);
    color: var(--paper);
  }

  .flow-node span,
  .flow-node small,
  .inspector-list dt {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    opacity: 0.72;
  }

  .flow-node strong {
    font-size: 13px;
    line-height: 1.2;
  }

  .agent-node {
    border-left-width: 4px;
  }

  .task-node {
    border-style: dashed;
  }

  .inspector-list {
    margin: 0;
    display: grid;
    gap: 10px;
  }

  .inspector-list div {
    display: grid;
    gap: 4px;
    padding-bottom: 8px;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .inspector-list div:last-child {
    border-bottom: none;
  }

  .inspector-list dd {
    margin: 0;
    color: var(--ink);
    font-size: 13px;
    line-height: 1.45;
  }

  .tool-palette {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .tool-chip {
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    padding: 7px 9px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
  }

  .tool-chip.active,
  .tool-chip:hover:not(:disabled) {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--paper);
  }

  .tool-chip:disabled {
    opacity: 0.42;
    cursor: not-allowed;
  }

  .builder-card {
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .conversational-card {
    /* No tinted gradient — riso surfaces are flat. The accent rule below
       distinguishes the conversational card from neighbours. */
    border-top: 4px solid var(--accent);
  }

  .extension-card {
    grid-column: 1 / -1;
  }

  .card-heading {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: start;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--ink);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    padding: 4px 8px;
    background: var(--paper);
    color: var(--ink);
    border: 1px solid var(--ink);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }

  .chip.subtle {
    border-color: var(--rule-soft);
    color: var(--ink-soft);
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--ink);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  input,
  textarea,
  button {
    font: inherit;
  }

  input,
  textarea,
  select {
    padding: 9px 10px;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
    font-size: 13px;
    letter-spacing: 0;
    text-transform: none;
  }

  input:focus,
  textarea:focus,
  select:focus {
    outline: none;
    background: rgba(240, 106, 58, 0.06);
  }

  textarea {
    resize: vertical;
    min-height: 92px;
  }

  .inline-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  button {
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    padding: 7px 14px;
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    cursor: pointer;
  }

  button:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.primary,
  button.activeChoice,
  .approve-btn {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

  button.primary:hover:not(:disabled),
  button.activeChoice:hover:not(:disabled),
  .approve-btn:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
    border-color: var(--ink);
  }

  .feedback {
    margin: 0;
    padding: 8px 12px;
    border-left: 3px solid var(--ink);
    background: rgba(14, 59, 102, 0.06);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1.4;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .feedback.error {
    border-left-color: var(--accent);
    background: rgba(240, 106, 58, 0.1);
    color: var(--ink-orange);
  }

  .feedback.success {
    border-left-color: var(--ink);
    background: rgba(14, 59, 102, 0.08);
    color: var(--ink);
  }

  .item-list,
  .stack {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .authoring-columns {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 14px;
  }

  .authoring-panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    background: rgba(14, 59, 102, 0.04);
    border: 1px solid var(--rule-soft);
  }

  .subheading {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: center;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .compact-list {
    max-height: 200px;
    overflow: auto;
  }

  .proposal-stack {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .proposal-summary {
    padding: 12px 14px;
    background: rgba(240, 106, 58, 0.08);
    border-left: 3px solid var(--accent);
  }

  .list-item {
    align-items: start;
    text-align: left;
    width: 100%;
  }

  .list-item.candidate {
    flex-direction: row;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
  }

  .reviewed-operation.blocked {
    border-left: 3px solid var(--accent);
    padding-left: 12px;
    background: rgba(240, 106, 58, 0.06);
  }

  .diagnostic-list {
    margin: 10px 0 0;
    padding-left: 18px;
    color: var(--ink-orange);
    font-family: var(--font-mono);
    font-size: 11.5px;
  }

  .diagnostic-list.compact {
    margin-top: 6px;
    font-size: 11px;
  }

  .approve-btn {
    white-space: nowrap;
  }

  .lens-list-button {
    all: unset;
    display: flex;
    flex-direction: column;
    gap: 4px;
    cursor: pointer;
    min-width: 0;
  }

  .lens-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .operator-shell {
    min-height: 0;
  }

  .operator-shell-state {
    padding: 14px 18px;
    border: 1px dashed var(--accent);
    color: var(--ink-orange);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  @media (max-width: 1100px) {
    .builder-module-row,
    .builder-grid,
    .authoring-columns {
      grid-template-columns: 1fr;
    }

    .flow-board {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @media (max-width: 980px) {
    .builder-hero {
      grid-template-columns: 1fr;
    }

    .builder-deck {
      padding: 16px;
    }

    .flow-board {
      grid-template-columns: 1fr;
    }
  }
</style>
