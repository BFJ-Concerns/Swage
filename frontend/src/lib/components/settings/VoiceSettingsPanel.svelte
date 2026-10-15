<script lang="ts">
  import { SETTINGS_LABELS } from "../../settings-labels";
  import type { SettingsResponse } from "../../types";
  interface Props { settings: SettingsResponse; }
  let { settings = $bindable() }: Props = $props();
  function changed(): void { settings = { ...settings }; }
  function setModel(key: "transcription_model" | "speech_model", value: string): void {
    settings.voice = { ...settings.voice, [key]: value.trim() || null };
    changed();
  }
</script>

<p class="prereq">Voice speaks to the default provider's endpoint, which must use the OpenAI protocol and serve the audio models named here. Leave a model empty to turn that capability off.</p>
<div class="field"><div class="key">{SETTINGS_LABELS["voice.transcription_model"]}<small>Model id sent with speech-to-text requests, such as whisper-1.</small></div><div class="val"><input type="text" class="text" aria-label={SETTINGS_LABELS["voice.transcription_model"]} value={settings.voice.transcription_model ?? ""} oninput={(event) => setModel("transcription_model", event.currentTarget.value)} placeholder="Not set" autocomplete="off"/></div></div>
<div class="field"><div class="key">{SETTINGS_LABELS["voice.speech_model"]}<small>Model id sent with text-to-speech requests, such as tts-1.</small></div><div class="val"><input type="text" class="text" aria-label={SETTINGS_LABELS["voice.speech_model"]} value={settings.voice.speech_model ?? ""} oninput={(event) => setModel("speech_model", event.currentTarget.value)} placeholder="Not set" autocomplete="off"/></div></div>

<style>
  .prereq{margin:0 0 12px;color:var(--ink-soft);font-size:13px;line-height:1.55}.field{display:grid;grid-template-columns:220px 1fr;gap:24px;border-top:1px solid var(--rule-soft);padding:14px 0;align-items:baseline}.key{font-family:var(--font-mono);font-weight:500;font-size:11px;line-height:1.5;letter-spacing:.18em;text-transform:uppercase}.key small{display:block;margin-top:4px;font-family:var(--font-body);font-weight:400;font-size:11px;line-height:1.4;letter-spacing:0;text-transform:none;color:var(--ink-soft)}.val{display:flex;gap:10px;flex-wrap:wrap}.text{width:100%;max-width:480px;padding:9px 11px;border:1px solid var(--ink);background:var(--paper);color:var(--ink);font-family:var(--font-body);font-size:13px}@media(max-width:760px){.field{grid-template-columns:1fr;gap:10px}}
</style>
