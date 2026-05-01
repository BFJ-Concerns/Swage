// Shared theme type for the riso/cyanotype shell.
//
// "system" follows the OS preference via prefers-color-scheme; light maps to
// .theme-riso, dark maps to .theme-cyanotype. App.svelte owns the mapping —
// this module just defines the vocabulary so child components and tests can
// type their props consistently.

export type ThemeMode = "light" | "dark" | "system";
