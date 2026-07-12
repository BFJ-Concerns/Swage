<!--
  StatusBadge — the one surface that turns a status register into colour.

  Six semantic registers name "what state is this thing in"; every caller maps
  its domain state to a register and never to a colour. This component is the
  SOLE reader of the --status-*-hue token family (riso.css) — no other surface
  reaches for the raw hues, so the palette has exactly one consumer to reason
  about and to verify (scripts/verify-status-hues.mjs).

  Two channels, layered, so colour is never the only cue:
    · TREATMENT (salience) — a solid stamp for the states that carry weight
      (calm present, wants-a-human, trouble), a quiet outline for the states
      that recede (in-motion, not-yet, elsewhere), a dashed outline for the
      foreign one, and a pulsing pip for the one that is actively moving.
    · HUE — a spot colour per register, held measurably clear (ΔE00) of the two
      warm roles the palette reserves (--accent owns action, --error owns
      trouble). calm reads ink and trouble reads the error red, so their
      salience and hue coincide.
  The label always renders, so a colour-blind reader loses nothing.
-->
<script module lang="ts">
  // The stable status vocabulary. Domain code maps a state to one of these;
  // it is a presentation concern, not a wire type — the server sends domain
  // states (a job's "active", a device's "connected"), and the caller registers
  // them here.
  export type Register =
    | "calm" // steady, present, all-good — the quiet default
    | "in-motion" // actively working right now
    | "wants-a-human" // needs attention or a decision
    | "not-yet" // dormant / not active yet / paused
    | "elsewhere" // external / remote / outside this system
    | "trouble"; // a real failure

  // Register → hue token + treatment. Kept as data (not per-register CSS) so
  // registers never passed at runtime raise no unused-selector warnings, and
  // the whole treatment table reads in one place.
  type Treatment = {
    hue: string; // a var() reference into the --status-*-hue family
    stamp: boolean; // solid fill (paper text) vs. outline (hue text)
    dashed?: boolean; // dashed edge — the "not ours" signal
    pip?: boolean; // a pulsing square — the "in motion" signal
  };

  const TREATMENTS: Record<Register, Treatment> = {
    calm: { hue: "var(--status-calm-hue)", stamp: true },
    "in-motion": { hue: "var(--status-in-motion-hue)", stamp: false, pip: true },
    "wants-a-human": { hue: "var(--status-wants-human-hue)", stamp: true },
    "not-yet": { hue: "var(--status-not-yet-hue)", stamp: false },
    elsewhere: { hue: "var(--status-elsewhere-hue)", stamp: false, dashed: true },
    trouble: { hue: "var(--status-trouble-hue)", stamp: true },
  };
</script>

<script lang="ts">
  interface Props {
    register: Register;
    // The visible text — the domain status the register stands in for.
    label: string;
  }

  let { register, label }: Props = $props();

  const treatment = $derived(TREATMENTS[register]);
</script>

<span
  class="badge"
  class:dashed={treatment.dashed}
  style:--badge-hue={treatment.hue}
  style:--badge-bg={treatment.stamp ? treatment.hue : "transparent"}
  style:--badge-fg={treatment.stamp ? "var(--paper)" : treatment.hue}
>
  {#if treatment.pip}
    <!-- Decorative live-activity pulse; the label carries the meaning. -->
    <span class="pip" aria-hidden="true"></span>
  {/if}
  {label}
</span>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 8px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    border: 1px solid var(--badge-hue);
    background: var(--badge-bg);
    color: var(--badge-fg);
  }

  /* The foreign register: a dashed edge reads as "provisional, not ours". */
  .badge.dashed {
    border-style: dashed;
  }

  /* in-motion's pulsing pip — a square, in keeping with the riso vocabulary
     (see ToolCallDisplay). The badge label stays at full contrast; only the
     pip animates, so the motion cue never costs legibility. */
  .pip {
    width: 6px;
    height: 6px;
    flex-shrink: 0;
    background: var(--badge-hue);
    animation: badge-pulse 1.4s ease-in-out infinite;
  }

  @keyframes badge-pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.3;
    }
  }

  /* Respect a reduced-motion preference — hold the pip steady. */
  @media (prefers-reduced-motion: reduce) {
    .pip {
      animation: none;
    }
  }
</style>
