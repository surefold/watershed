import { resolve } from "node:path";
import { defaultClientConditions, defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The React example. Built into web/dist/grid/react, under the Solid page.
export default defineConfig({
  base: "./",
  // Use the packages' TypeScript source from this repository, not their built dist/.
  resolve: { conditions: ["source", ...defaultClientConditions] },
  plugins: [react()],
  // The table's worker is a module, with a top-level await for its engine.
  worker: { format: "es" },
  build: {
    target: "es2022",
    outDir: "../dist/grid/react",
    emptyOutDir: false,
    // The page, and the cookbook under it.
    rollupOptions: { input: { main: resolve(__dirname, "index.html"), cookbook: resolve(__dirname, "cookbook/index.html") } },
  },
  esbuild: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
});
