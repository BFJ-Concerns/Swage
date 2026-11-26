import { createServer } from "node:net";

/**
 * Choose distinct loopback ports, holding every socket until all are chosen.
 * The caller must start its servers after this returns; these reservations
 * prevent duplicate choices within the batch, not competing processes.
 * @param {number} count
 * @returns {Promise<number[]>}
 */
export async function unusedPorts(count) {
  if (!Number.isInteger(count) || count < 1) throw new RangeError("Port count must be a positive integer");
  /** @type {import("node:net").Server[]} */
  const reservations = [];
  try {
    const ports = [];
    for (let index = 0; index < count; index += 1) {
      const server = createServer();
      reservations.push(server);
      await new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(0, "127.0.0.1", () => {
          server.removeListener("error", reject);
          resolve(undefined);
        });
      });
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("Port reservation has no TCP address");
      ports.push(address.port);
    }
    return ports;
  } finally {
    await Promise.all(reservations.filter((server) => server.listening).map((server) =>
      new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve(undefined))),
    ));
  }
}
