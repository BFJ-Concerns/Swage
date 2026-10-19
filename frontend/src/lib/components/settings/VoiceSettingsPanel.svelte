<script lang="ts">
  import { SETTINGS_LABELS } from "../../settings-labels";
  import type { SettingsResponse } from "../../types";
  import { checkSpeechModel } from "../../api/workflows";
  interface Props {
    settings: SettingsResponse;
    onsave: () => Promise<boolean>;
    saving: boolean;
  }
  let { settings = $bindable(), onsave, saving }: Props = $props();
  let checking = $state(false);
  let result = $state<{ model: string; error: boolean; message: string } | null>(null);
  function changed(): void { settings = { ...settings }; }
  function setModel(key: "transcription_model" | "speech_model", value: string): void {
    result = null;
    settings.voice = { ...settings.voice, [key]: value.trim() || null };
    changed();
  }
  async function checkSpeech(): Promise<void> {
    if (checking || saving) return;
    checking = true;
    result = null;
    try {
      if (!(await onsave())) return;
      const model = settings.voice.speech_model;
      if (!model) return;
      try {
        await checkSpeechModel();
        result = { model, error: false, message: `Speech model ${model} is working.` };
      } catch (error) {
        result = { model, error: true, message: error instanceof Error ? error.message : "Speech model check failed. Check the model and default provider, then try again." };
      }
    } finally {
      checking = false;
    }
  }
</script>

<p class="prereq">Voice speaks to the default provider's endpoint, which must use the OpenAI protocol and serve the audio models named here. Leave a model empty to turn that capability off.</p>
<div class="field"><div class="key">{SETTINGS_LABELS["voice.transcription_model"]}<small>Model id sent with speech-to-text requests, such as whisper-1.</small></div><div class="val"><input type="text" class="text" aria-label={SETTINGS_LABELS["voice.transcription_model"]} disabled={checking || saving} value={settings.voice.transcription_model ?? ""} oninput={(event) => setModel("transcription_model", event.currentTarget.value)} placeholder="Not set" autocomplete="off"/></div></div>
<div class="field"><div class="key">{SETTINGS_LABELS["voice.speech_model"]}<small>Model id sent with text-to-speech requests, such as tts-1.</small></div><div class="val speech"><input type="text" class="text" aria-label={SETTINGS_LABELS["voice.speech_model"]} disabled={checking || saving} value={settings.voice.speech_model ?? ""} oninput={(event) => setModel("speech_model", event.currentTarget.value)} placeholder="Not set" autocomplete="off"/>
  <button type="button" class="btn" disabled={checking || saving || !settings.voice.speech_model} onclick={checkSpeech}>{checking ? "Checking speech…" : "Check speech model"}</button>
  <small>The check saves these settings and requests a short audio sample from the default provider. Provider charges may apply.</small>
  {#if result && result.model === settings.voice.speech_model}
    <p class="feedback" class:error={result.error} role={result.error ? "alert" : "status"}>{result.message}</p>
  {/if}
</div></div>

<style>
  .prereq{margin:0 0 12px;color:var(--ink-soft);font-size:13px;line-height:1.55}.field{display:grid;grid-template-columns:220px 1fr;gap:24px;border-top:1px solid var(--rule-soft);padding:14px 0;align-items:baseline}.key{font-family:var(--font-mono);font-weight:500;font-size:11px;line-height:1.5;letter-spacing:.18em;text-transform:uppercase}.key small{display:block;margin-top:4px;font-family:var(--font-body);font-weight:400;font-size:11px;line-height:1.4;letter-spacing:0;text-transform:none;color:var(--ink-soft)}.val{display:flex;gap:10px;flex-wrap:wrap}.text{width:100%;max-width:480px;padding:9px 11px;border:1px solid var(--ink);background:var(--paper);color:var(--ink);font-family:var(--font-body);font-size:13px}@media(max-width:760px){.field{grid-template-columns:1fr;gap:10px}}
  .speech { flex-direction: column; align-items: flex-start; }
  .btn { background: var(--paper); color: var(--ink); border: 1px solid var(--ink); padding: 8px 16px; font-family: var(--font-display); font-size: 14px; line-height: 1; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; }
  .btn:hover:not(:disabled) { background: var(--ink); color: var(--paper); }
  .btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .feedback { margin: 0; }
  .feedback.error { color: var(--error); }
</style>
