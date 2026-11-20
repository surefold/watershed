import { resolve } from "node:path";
import { defaultClientConditions, defineConfig } from "vite";
import solid from "@solidjs/vite-plugin";

// The site's grid page. Built into web/dist/grid, beside the playground.
export default defineConfig({
  base: "./",
  // Use the packages' TypeScript source from this repository, not their built dist/.
  resolve: { conditions: ["source", ...defaultClientConditions] },
  plugins: [solid()],
  // The table's worker is a module, with a top-level await for its engine.
  worker: { format: "es" },
  build: {
    target: "es2022",
    outDir: "../dist/grid",
    emptyOutDir: false,
    // The page, and the cookbook under it.
    rollupOptions: { input: { main: resolve(import.meta.dirname, "index.html"), cookbook: resolve(import.meta.dirname, "cookbook/index.html") } },
  },
});
