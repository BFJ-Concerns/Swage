// @ts-expect-error Vitest supplies Node APIs; the application has browser types.
import { readFileSync } from "node:fs";

const themeCss = readFileSync("src/lib/styles/riso.css", "utf8");

/** Apply production CSS with theme tokens resolved for jsdom's CSS engine. */
export function renderedStyles(componentCss = ""): HTMLStyleElement {
  const style = document.createElement("style");
  style.textContent = themeCss + componentCss;
  document.head.append(style);
  // jsdom does not resolve custom properties inside border shorthands.
  // Read the real theme's root values rather than supplying test colours.
  const theme = getComputedStyle(document.documentElement);
  style.textContent = style.textContent.replace(/var\((--[\w-]+)\)/g, (original, token) =>
    theme.getPropertyValue(token).trim() || original,
  );
  return style;
}
