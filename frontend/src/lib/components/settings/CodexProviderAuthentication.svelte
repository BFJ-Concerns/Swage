<script lang="ts">
  import type { ProviderSignInState } from "../../protocol/ProviderSignInState";

  interface Props {
    providerId: string;
    saved: boolean;
    state: ProviderSignInState | undefined;
    deviceCode: { code: string; verificationUrl: string } | null;
    onbegin: (id: string, method: "browser" | "device_code") => void;
    oncheck: (id: string) => void;
    oncopy: () => void;
    onend: (id: string) => void;
  }

  let { providerId, saved, state, deviceCode, onbegin, oncheck, oncopy, onend }: Props = $props();
</script>

<div class="field">
  <div class="key">ChatGPT account<small>Codex manages the browser or device-code sign-in and refreshes it automatically.</small></div>
  <div class="val val-stack">
    {#if state?.status === "signed_in"}
      <span class="credential-status set">Signed in{state.account?.email ? ` as ${state.account.email}` : ""}</span>
      <button type="button" class="secondary" onclick={() => onend(providerId)}>Sign out</button>
    {:else}
      {#if state?.status === "signing_in"}
        <p class="sign-in-guidance">Complete sign-in in the opened tab. Swage checks automatically; use “Check now” to finish immediately.</p>
      {:else if state?.status === "expired"}
        <p class="sign-in-guidance warning">Your ChatGPT sign-in has expired. Sign in again to use Codex models.</p>
      {/if}
      <div class="opt-row">
        <button type="button" class="secondary" disabled={!saved} onclick={() => onbegin(providerId, "browser")}>Sign in with browser</button>
        <button type="button" class="secondary" disabled={!saved} onclick={() => onbegin(providerId, "device_code")}>Use device code</button>
        <button type="button" class="secondary" disabled={!saved} onclick={() => oncheck(providerId)}>Check now</button>
      </div>
      {#if !saved}<small>Save settings before signing in.</small>{/if}
      {#if deviceCode}
        <div class="device-code-instruction">
          <span>Enter this device code on the verification surface opened in your browser:</span>
          <div class="opt-row">
            <label><span class="sr-only">Device code</span><input class="text device-code" readonly value={deviceCode.code} /></label>
            <button type="button" class="secondary" onclick={oncopy}>Copy code</button>
            <a href={deviceCode.verificationUrl} target="_blank" rel="noreferrer">Open verification surface</a>
          </div>
        </div>
      {/if}
    {/if}
  </div>
</div>

<style>
  .field { display: grid; grid-template-columns: 220px 1fr; gap: 24px; border-top: 1px solid var(--rule-soft); padding: 14px 0; align-items: baseline; }
  .key { font-family: var(--font-mono); font-weight: 500; font-size: 11px; line-height: 1.5; letter-spacing: 0.18em; text-transform: uppercase; }
  .key small { display: block; margin-top: 4px; font-family: var(--font-body); font-weight: 400; font-size: 11px; line-height: 1.4; letter-spacing: 0; text-transform: none; color: var(--ink-soft); }
  .val { display: flex; gap: 10px; flex-wrap: wrap; }
  .val-stack { flex-direction: column; align-items: stretch; gap: 12px; }
  .credential-status { font-family: var(--font-mono); font-weight: 500; font-size: 10px; letter-spacing: 0.16em; text-transform: uppercase; padding: 3px 8px; border: 1px solid var(--ink-soft); color: var(--ink-soft); }
  .credential-status.set { border-color: var(--ink); color: var(--ink); background: color-mix(in srgb, var(--success) 14%, transparent); }
  .sign-in-guidance { margin: 0; color: var(--ink-soft); }
  .sign-in-guidance.warning { color: var(--ink-orange); }
  .device-code-instruction { display: grid; gap: 8px; }
  .opt-row { display: flex; gap: 8px; flex-wrap: wrap; }
  .text { width: 100%; max-width: 480px; padding: 9px 11px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); font-family: var(--font-body); font-size: 13px; line-height: 1.4; }
  .device-code { width: 14ch; font-family: var(--font-mono); letter-spacing: 0.08em; }
  .text:focus { outline: none; background: color-mix(in srgb, var(--accent) 6%, transparent); }
  .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
  @media (max-width: 760px) { .field { grid-template-columns: 1fr; gap: 10px; } }
</style>
