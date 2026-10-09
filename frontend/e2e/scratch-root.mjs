// Where browser E2E runs create their disposable state and results.
// SWAGE_TEST_SCRATCH_DIR names the parent directory, for a machine whose
// system temporary directory is too small for test state; unset, the system
// temporary directory is used.
import { mkdir, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const scratchParent = process.env.SWAGE_TEST_SCRATCH_DIR ?? tmpdir();

/**
 * Create a fresh directory under the scratch parent.
 * @param {string} prefix
 * @returns {Promise<string>}
 */
export async function makeScratchDir(prefix) {
  await mkdir(scratchParent, { recursive: true });
  return mkdtemp(join(scratchParent, prefix));
}
