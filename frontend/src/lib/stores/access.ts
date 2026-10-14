// Whether the gateway has refused this client for want of its token.
//
// A gateway bound beyond loopback admits paired clients only: the first
// refused request flips this store, the pairing prompt asks for the one-time
// code the operator issued, and a successful redemption sets the cookie the
// gateway reads thereafter. The API layer sets it; the prompt clears it.

import { writable } from "svelte/store";

export const pairingRequired = writable(false);

export function markPairingRequired(): void {
  pairingRequired.set(true);
}

export function markPaired(): void {
  pairingRequired.set(false);
}
