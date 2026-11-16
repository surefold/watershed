import { resolve } from "node:path";
import { defaultClientConditions, defineConfig } from "vite";

// The pages the tests drive, built into web/dist/e2e beside the grid's pages
// so that one server serves both. They are not part of the site: web/build.sh
// starts web/dist clean.
export default defineConfig({
  root: resolve(__dirname, "fixtures"),
  base: "./",
  // Use the packages' TypeScript source from this repository, not their built dist/.
  resolve: { conditions: ["source", ...defaultClientConditions] },
  // The table's worker is a module, with a top-level await for its engine.
  worker: { format: "es" },
  build: {
    target: "es2022",
    outDir: resolve(__dirname, "../dist/e2e"),
    emptyOutDir: true,
    rollupOptions: { input: { onedit: resolve(__dirname, "fixtures/onedit.html") } },
  },
  esbuild: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
});
