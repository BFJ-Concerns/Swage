// @ts-expect-error Vitest supplies Node APIs; the application has browser types.
import { readFileSync } from "node:fs";
// @ts-expect-error Vitest supplies Node APIs; the application has browser types.
import { resolve } from "node:path";
// @ts-expect-error Vitest supplies Node APIs; the application has browser types.
import { cwd } from "node:process";
import { expect, it } from "vitest";
import page from "../../../index.html?raw";

it("loads every UI font family and weight from bundled font files", () => {
  const document = new DOMParser().parseFromString(page, "text/html");
  const remoteLinks = [...document.querySelectorAll("link[href]")].filter(
    (link) => /^https?:\/\//.test(link.getAttribute("href")!),
  );
  expect(remoteLinks.map((link) => link.getAttribute("href"))).toEqual([]);
  const faces = [...page.matchAll(/@font-face\s*\{([^}]+)\}/g)].map(
    ([, rule]) => {
      const family = rule.match(/font-family:\s*"([^"]+)"/)?.[1];
      const weight = rule.match(/font-weight:\s*([^;]+)/)?.[1];
      const source = rule.match(/url\("([^"]+)"\)/)?.[1];
      expect(source).toMatch(/^\/fonts\//);
      const bytes = readFileSync(
        resolve(cwd(), "public", source!.slice(1)),
      );
      expect(bytes.subarray(0, 4).toString("hex")).toBe("00010000");
      expect(bytes.length).toBeGreaterThan(1000);
      return [family, weight];
    },
  );
  expect(faces).toEqual([
    ["Bebas Neue", "400"],
    ["Hanken Grotesk", "400 700"],
    ["DM Mono", "400"],
    ["DM Mono", "500"],
  ]);
});
