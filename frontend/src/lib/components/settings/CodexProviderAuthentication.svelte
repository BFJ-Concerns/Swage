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
          <span>Enter this device code on the verification page opened in your browser:</span>
          <div class="opt-row">
            <label><span class="sr-only">Device code</span><input class="text device-code" readonly value={deviceCode.code} /></label>
            <button type="button" class="secondary" onclick={oncopy}>Copy code</button>
            <a href={deviceCode.verificationUrl} target="_blank" rel="noreferrer">Open verification page</a>
          </div>
        </div>
      {/if}
    {/if}
  </div>
</div>
