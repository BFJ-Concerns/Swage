import { builtinEnvironments } from "vitest/runtime";
import type { Environment } from "vitest/runtime";

// jsdom 29 delegates WebSockets to Undici's Node EventTarget. Undici's
// connection callback constructs Event from the global at delivery time;
// keeping Node's class prevents a cross-realm Event rejection. DOM clicks
// still construct their own events inside jsdom.
export default {
  ...builtinEnvironments.jsdom,
  name: "network-dom",
  async setup(global, options) {
    const NativeEvent = global.Event;
    const environment = await builtinEnvironments.jsdom.setup(global, options);
    global.Event = NativeEvent;
    return environment;
  },
} satisfies Environment;
