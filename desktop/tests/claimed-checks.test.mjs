import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const smoke = fileURLToPath(new URL("./smoke.mjs", import.meta.url));

function runSmoke(env) {
  const result = spawnSync(process.execPath, [smoke], {
    env,
    encoding: "utf8",
    timeout: 120_000,
    maxBuffer: 4 * 1024 * 1024,
  });
  const log = `${result.stdout ?? ""}${result.stderr ?? ""}`;
  console.log(log);
  assert.ifError(result.error);
  assert.equal(result.status, 0, log);
  assert.match(log, /browser smoke: start/);
  assert.match(log, /browser smoke: done/);
  return log;
}

test("Electron names its no-display fallback", () => {
  const env = { ...process.env };
  delete env.DISPLAY;
  delete env.WAYLAND_DISPLAY;
  const log = runSmoke(env);
  assert.match(log, /electron smoke: skipped \(no display server\)/);
  assert.doesNotMatch(log, /electron smoke: launched/);
});

test("Electron launches and completes on the supplied display", () => {
  const log = runSmoke(process.env);
  if (!process.env.DISPLAY && !process.env.WAYLAND_DISPLAY) {
    assert.match(log, /electron smoke: skipped \(no display server\)/);
    console.log("Electron display journey unavailable: run under xvfb-run");
    return;
  }
  assert.match(log, /electron smoke: launched/);
  assert.match(log, /electron smoke: first window/);
  assert.match(log, /electron smoke: done/);
  assert.doesNotMatch(log, /electron smoke: skipped/);
});
