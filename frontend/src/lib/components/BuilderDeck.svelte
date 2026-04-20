<script lang="ts">
  import {
    approveWorkspaceExtension,
    createProjectExtension,
    createTeam,
    listProjectExtensions,
    listTeams,
    updateProjectExtension,
    updateTeam,
  } from "../api/builder";
  import { createLens, installLensPreset, updateLens } from "../api/lens";
  import SharedReferenceStudio from "./SharedReferenceStudio.svelte";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { ProjectStore } from "../stores/projects";
  import type {
    AppLens,
    LensDescriptor,
    LensSurface,
    ProjectExtensionSummary,
    TeamSummary,
  } from "../types";

  type OperatorDashboardModule = typeof import("./OperatorDashboard.svelte");

  interface Props {
    projectStore?: ProjectStore;
    desktopRuntime?: DesktopRuntimeInfo | null;
    activeLens: AppLens;
    availableLenses: LensDescriptor[];
    onSelectLens: (next: AppLens) => void | Promise<void>;
    onRefreshLenses: (preferredSurface?: LensSurface) => void | Promise<void>;
  }

  let {
    projectStore,
    desktopRuntime,
    activeLens,
    availableLenses,
    onSelectLens,
    onRefreshLenses,
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
  let teamMembersCsv = $state("");

  let lensError = $state<string | null>(null);
  let lensNotice = $state<string | null>(null);
  let selectedLensId = $state<string | null>(null);
  let lensId = $state("");
  let lensTitle = $state("");
  let lensSummary = $state("");
  let lensDefaultSurface = $state<LensSurface>("conversation");
  let lensModulesCsv = $state("conversation, operator");
  let lensProminentTeamsCsv = $state("default");

  let extensions = $state<ProjectExtensionSummary[]>([]);
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

  let lastLoadedProjectId = "";
  let operatorDashboardModule = $state<Promise<OperatorDashboardModule> | null>(null);

  const currentProject = $derived(projectStore?.currentProject ?? null);
  const managedExtensions = $derived(
    extensions.filter((extension) => extension.source_kind === "managed"),
  );
  const workspaceCandidates = $derived(
    extensions.filter((extension) => extension.status === "approval_required"),
  );

  $effect(() => {
    if (!operatorDashboardModule) {
      operatorDashboardModule = import("./OperatorDashboard.svelte");
    }
  });

  $effect(() => {
    const projectId = currentProject?.id ?? "";
    if (!projectId || projectId === lastLoadedProjectId) {
      return;
    }
    lastLoadedProjectId = projectId;
    void refreshBuilderData(projectId);
  });

  function normaliseCsvList(value: string): string[] {
    return value
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);
  }

  function resetTeamForm(): void {
    selectedTeamId = null;
    teamName = "";
    teamDescription = "";
    teamEntryAgentId = "default";
    teamMembersCsv = "";
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
    teamMembersCsv = team.members.join(", ");
  }

  function resetExtensionForm(): void {
    selectedExtensionName = null;
    extensionName = "";
    extensionDisplayName = "";
    extensionDescription = "";
    extensionToolsCsv = "";
    extensionInstructions =
      "Describe the builder behaviour this extension should inject.";
  }

  function populateExtensionForm(extension: ProjectExtensionSummary): void {
    selectedExtensionName = extension.name;
    extensionName = extension.name;
    extensionDisplayName = extension.display_name ?? "";
    extensionDescription = extension.description;
    extensionToolsCsv = extension.requested_tools.join(", ");
    extensionInstructions = extension.instructions;
  }

  async function refreshBuilderData(projectId: string): Promise<void> {
    isLoading = true;
    loadError = null;
    try {
      const [nextTeams, nextExtensions] = await Promise.all([
        listTeams(projectId),
        listProjectExtensions(projectId),
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
    if (!currentProject) return;
    teamError = null;
    teamNotice = null;

    try {
      const payload = {
        name: teamName,
        description: teamDescription,
        entry_agent_id: teamEntryAgentId,
        members: normaliseCsvList(teamMembersCsv),
      };

      const saved = selectedTeamId
        ? await updateTeam(selectedTeamId, payload)
        : await createTeam(currentProject.id, payload);

      teamNotice = selectedTeamId
        ? `Updated team ${saved.name}.`
        : `Created team ${saved.name}.`;
      await refreshBuilderData(currentProject.id);
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
    if (!currentProject) return;
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
        ? await updateProjectExtension(currentProject.id, selectedExtensionName, payload)
        : await createProjectExtension(currentProject.id, {
            name: extensionName,
            ...payload,
          });

      extensionNotice = selectedExtensionName
        ? `Updated extension ${saved.name}.`
        : `Created extension ${saved.name}.`;
      await refreshBuilderData(currentProject.id);
      populateExtensionForm(saved);
    } catch (error) {
      extensionError =
        error instanceof Error ? error.message : "Failed to save extension.";
    }
  }

  async function importWorkspaceExtension(name: string): Promise<void> {
    if (!currentProject) return;
    extensionError = null;
    extensionNotice = null;

    try {
      const imported = await approveWorkspaceExtension(currentProject.id, name);
      extensionNotice = `Approved workspace extension ${imported.name}.`;
      await refreshBuilderData(currentProject.id);
      populateExtensionForm(imported);
    } catch (error) {
      extensionError =
        error instanceof Error ? error.message : "Failed to approve extension.";
    }
  }

  async function handleTeamTopologyUpdate(): Promise<void> {
    if (!currentProject) return;
    await refreshBuilderData(currentProject.id);
  }
</script>

<section class="builder-deck">
  <header class="builder-hero">
    <div class="hero-copy">
      <p class="eyebrow">Builder Lens</p>
      <h1>Author the workspace, then let the agent live in it.</h1>
      <p class="lede">
        Builder mode now edits the durable shape of a project: which team leads
        it, which extension bundles are trusted, and which presentation lens
        should take over next.
      </p>
    </div>

    <div class="status-board">
      <article class="status-card">
        <span class="label">Current project</span>
        <strong>{currentProject?.name ?? "No project selected"}</strong>
        <span class="meta mono">
          {currentProject?.workspace_path ?? "workspace pending"}
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
        <span class="label">Authoring posture</span>
        <strong>{managedExtensions.length} managed extension(s)</strong>
        <span class="meta">
          {workspaceCandidates.length} workspace candidate(s) waiting for trust.
        </span>
      </article>
    </div>
  </header>

  {#if !currentProject}
    <section class="empty-state">
      Select a project before editing teams or extensions.
    </section>
  {:else}
    <section class="builder-grid">
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
          Shape the durable team that a project exposes for topology runs and
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
          <span>Entry agent ID</span>
          <input bind:value={teamEntryAgentId} placeholder="default" />
        </label>
        <label>
          <span>Members</span>
          <input bind:value={teamMembersCsv} placeholder="default, reviewer" />
        </label>
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
              <span>{team.entry_agent_id}</span>
            </button>
          {/each}
        </div>
      </article>

      <article class="builder-card extension-card">
        <div class="card-heading">
          <div>
            <p class="section-kicker">Extension</p>
            <h2>Project extension bundles</h2>
          </div>
          <span class="chip">{managedExtensions.length} active</span>
        </div>
        <p class="section-copy">
          Managed extensions are written into the project config tree and load
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

      <SharedReferenceStudio
        projectId={currentProject.id}
        onTeamTopologyUpdated={handleTeamTopologyUpdate}
      />
    </section>

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
</section>

<style>
  .builder-deck {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 22px;
    min-height: 0;
    overflow: auto;
    background:
      radial-gradient(circle at top left, rgba(251, 191, 36, 0.12), transparent 34%),
      linear-gradient(180deg, rgba(255, 252, 246, 0.98), rgba(245, 240, 231, 0.96));
  }

  .builder-hero {
    display: grid;
    grid-template-columns: minmax(0, 1.25fr) minmax(320px, 0.95fr);
    gap: 18px;
    align-items: stretch;
  }

  .hero-copy,
  .status-board,
  .builder-card {
    border: 1px solid rgba(120, 53, 15, 0.12);
    border-radius: 24px;
    background:
      linear-gradient(180deg, rgba(255, 252, 246, 0.96), rgba(248, 244, 235, 0.94));
    box-shadow:
      0 24px 50px rgba(120, 53, 15, 0.06),
      inset 0 1px 0 rgba(255, 255, 255, 0.78);
  }

  .hero-copy {
    padding: 28px;
  }

  .eyebrow,
  .section-kicker {
    margin: 0 0 8px;
    font-size: 0.78rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: #9a3412;
    font-weight: 700;
  }

  .section-kicker {
    margin-bottom: 4px;
  }

  h1 {
    margin: 0;
    font-size: clamp(2rem, 3vw, 3rem);
    line-height: 1.02;
    color: #1f2937;
  }

  h2,
  h3 {
    margin: 0;
    color: #111827;
  }

  .lede,
  .section-copy {
    margin: 14px 0 0;
    max-width: 44rem;
    font-size: 0.98rem;
    line-height: 1.6;
    color: #4b5563;
  }

  .status-board {
    display: grid;
    gap: 12px;
    padding: 18px;
  }

  .status-card,
  .list-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 14px 16px;
    border-radius: 18px;
    background: rgba(255, 255, 255, 0.74);
    border: 1px solid rgba(148, 163, 184, 0.18);
  }

  .label {
    font-size: 0.78rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: #9a3412;
  }

  strong {
    font-size: 1.02rem;
    color: #111827;
  }

  .meta,
  .muted-note,
  .list-item span {
    color: #6b7280;
    font-size: 0.86rem;
  }

  .mono,
  code {
    font-family: "SFMono-Regular", "Menlo", "Monaco", monospace;
  }

  .empty-state {
    padding: 28px;
    border-radius: 24px;
    border: 1px dashed rgba(120, 53, 15, 0.2);
    color: #7c2d12;
    background: rgba(255, 251, 235, 0.72);
  }

  .builder-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 18px;
  }

  .builder-card {
    padding: 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .extension-card {
    grid-column: 1 / -1;
  }

  .card-heading {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: start;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    padding: 6px 10px;
    border-radius: 999px;
    background: rgba(154, 52, 18, 0.1);
    color: #7c2d12;
    font-size: 0.78rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: #374151;
    font-size: 0.88rem;
    font-weight: 600;
  }

  input,
  textarea,
  button {
    font: inherit;
  }

  input,
  textarea,
  select {
    padding: 11px 12px;
    border-radius: 14px;
    border: 1px solid rgba(148, 163, 184, 0.32);
    background: rgba(255, 255, 255, 0.88);
    color: #111827;
  }

  textarea {
    resize: vertical;
    min-height: 92px;
  }

  .inline-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }

  button {
    border: 1px solid rgba(154, 52, 18, 0.16);
    border-radius: 999px;
    padding: 10px 14px;
    background: rgba(255, 255, 255, 0.82);
    color: #6b7280;
    cursor: pointer;
    font-weight: 600;
  }

  button.primary,
  button.activeChoice,
  .approve-btn {
    background: #1f2937;
    color: #fffaf0;
    border-color: #1f2937;
  }

  .feedback {
    margin: 0;
    padding: 10px 12px;
    border-radius: 14px;
    font-size: 0.88rem;
  }

  .feedback.error {
    background: rgba(254, 226, 226, 0.8);
    color: #991b1b;
  }

  .feedback.success {
    background: rgba(220, 252, 231, 0.8);
    color: #166534;
  }

  .item-list,
  .stack {
    display: flex;
    flex-direction: column;
    gap: 10px;
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

  .chip.subtle {
    background: rgba(15, 23, 42, 0.08);
    color: #334155;
  }

  .operator-shell {
    min-height: 0;
  }

  .operator-shell-state {
    padding: 18px;
    color: #7c2d12;
    font-weight: 600;
  }

  @media (max-width: 1100px) {
    .builder-grid {
      grid-template-columns: 1fr;
    }
  }

  @media (max-width: 980px) {
    .builder-hero {
      grid-template-columns: 1fr;
    }

    .builder-deck {
      padding: 16px;
    }
  }
</style>
