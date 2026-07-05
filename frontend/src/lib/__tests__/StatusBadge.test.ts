// @vitest-environment jsdom
/**
 * Tests for StatusBadge — the register → treatment mapping.
 *
 * The badge is the sole reader of the --status-*-hue tokens; these tests pin
 * the mapping from a register to its treatment (stamp vs. outline, dashed edge,
 * pulsing pip) and to the hue token it reads, so a future edit can't silently
 * repaint a register or drop a cue. Actual colour values live in riso.css and
 * are gated by the verification script, not here.
 */
import { describe, it, expect, afterEach } from "vitest";
import { mount, unmount } from "svelte";
import StatusBadge from "../components/StatusBadge.svelte";

type MountedComponent = ReturnType<typeof mount>;

let mounted: MountedComponent | null = null;

function render(props: { register: string; label: string }): HTMLElement {
  const target = document.createElement("div");
  document.body.appendChild(target);
  // register is typed as Register in the component; the string is fine at runtime.
  mounted = mount(StatusBadge, { target, props: props as never });
  return target;
}

afterEach(() => {
  if (mounted) {
    unmount(mounted);
    mounted = null;
  }
  document.body.replaceChildren();
});

describe("StatusBadge", () => {
  it("renders the label text for every register", () => {
    const target = render({ register: "calm", label: "connected" });
    expect(target.textContent).toContain("connected");
  });

  it("paints stamp registers with a solid hue fill and paper text", () => {
    const badge = render({ register: "wants-a-human", label: "interrupted" })
      .querySelector(".badge") as HTMLElement;
    // A stamp fills with the register hue and reverses the text to paper.
    expect(badge.style.getPropertyValue("--badge-bg")).toBe("var(--status-wants-human-hue)");
    expect(badge.style.getPropertyValue("--badge-fg")).toBe("var(--paper)");
  });

  it("paints outline registers transparent with hue text", () => {
    const badge = render({ register: "not-yet", label: "paused" })
      .querySelector(".badge") as HTMLElement;
    expect(badge.style.getPropertyValue("--badge-bg")).toBe("transparent");
    expect(badge.style.getPropertyValue("--badge-fg")).toBe("var(--status-not-yet-hue)");
  });

  it("gives in-motion a pulsing pip and no other register one", () => {
    const inMotion = render({ register: "in-motion", label: "running" });
    expect(inMotion.querySelector(".pip")).not.toBeNull();
    unmount(mounted!);
    mounted = null;

    const calm = render({ register: "calm", label: "connected" });
    expect(calm.querySelector(".pip")).toBeNull();
  });

  it("marks the elsewhere register with a dashed edge", () => {
    const elsewhere = render({ register: "elsewhere", label: "remote" })
      .querySelector(".badge") as HTMLElement;
    expect(elsewhere.classList.contains("dashed")).toBe(true);

    unmount(mounted!);
    mounted = null;
    const notYet = render({ register: "not-yet", label: "paused" })
      .querySelector(".badge") as HTMLElement;
    expect(notYet.classList.contains("dashed")).toBe(false);
  });

  it("reads the error red for trouble via the token alias", () => {
    const badge = render({ register: "trouble", label: "failed" })
      .querySelector(".badge") as HTMLElement;
    expect(badge.style.getPropertyValue("--badge-bg")).toBe("var(--status-trouble-hue)");
  });
});
