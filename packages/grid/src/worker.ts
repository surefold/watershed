// The worker that holds a WorkerTable's rows. Its engine loads with the
// module that uses it, which waits for the WebAssembly; messages that came
// meanwhile would find no one listening, so this listens at once and
// hands them on, in order, when the engine is ready.

import type { ToWorker } from "./protocol.js";

const host = import("./worker-host.js");

self.onmessage = async (event: MessageEvent<ToWorker>) => {
  (await host).handle(event.data);
};
