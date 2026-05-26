import type { ThemeMode } from "./theme";

export type DisplayDensity = "compact" | "comfortable";
export type MessageModelDisplay = "off" | "assistant" | "all";
export type ConversationRendering = "markdown" | "plain";
export type PrintMarks = "full" | "minimal" | "off";

export interface DisplayPreferences {
  theme: ThemeMode;
  density: DisplayDensity;
  message_model_display: MessageModelDisplay;
  conversation_rendering: ConversationRendering;
  print_marks: PrintMarks;
}

export const DISPLAY_PREFERENCES_KEY = "swage:display-preferences";
export const LEGACY_THEME_KEY = "swage:theme";
export const DISPLAY_PREFERENCES_CHANGED = "swage:display-preferences-changed";

export const DEFAULT_DISPLAY_PREFERENCES: DisplayPreferences = {
  theme: "system",
  density: "compact",
  message_model_display: "off",
  conversation_rendering: "markdown",
  print_marks: "full",
};

export function readDisplayPreferences(): DisplayPreferences {
  const stored = readStoredObject();
  const legacyTheme = readLegacyTheme();
  return {
    theme: readTheme(stored?.theme, legacyTheme),
    density: readOption(stored?.density, ["compact", "comfortable"], "compact"),
    message_model_display: readOption(
      stored?.message_model_display,
      ["off", "assistant", "all"],
      "off",
    ),
    conversation_rendering: readOption(
      stored?.conversation_rendering,
      ["markdown", "plain"],
      "markdown",
    ),
    print_marks: readOption(stored?.print_marks, ["full", "minimal", "off"], "full"),
  };
}

export function writeDisplayPreferences(preferences: DisplayPreferences): void {
  if (typeof localStorage !== "undefined") {
    localStorage.setItem(DISPLAY_PREFERENCES_KEY, JSON.stringify(preferences));
    localStorage.setItem(LEGACY_THEME_KEY, preferences.theme);
  }

  if (typeof window !== "undefined") {
    window.dispatchEvent(
      new CustomEvent<DisplayPreferences>(DISPLAY_PREFERENCES_CHANGED, {
        detail: preferences,
      }),
    );
  }
}

function readStoredObject(): Partial<DisplayPreferences> | null {
  if (typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(DISPLAY_PREFERENCES_KEY);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    return typeof parsed === "object" && parsed !== null ? parsed : null;
  } catch {
    return null;
  }
}

function readLegacyTheme(): ThemeMode | null {
  if (typeof localStorage === "undefined") return null;
  return readTheme(localStorage.getItem(LEGACY_THEME_KEY), null);
}

function readTheme(value: unknown, fallback: ThemeMode | null): ThemeMode {
  return readOption(value, ["light", "dark", "system"], fallback ?? "system");
}

function readOption<T extends string>(
  value: unknown,
  options: readonly T[],
  fallback: T,
): T {
  return typeof value === "string" && options.includes(value as T) ? (value as T) : fallback;
}
