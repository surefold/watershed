import { defaultClientConditions, defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The React example. Built into web/dist/grid/react, under the Solid page.
export default defineConfig({
  base: "./",
  // Use the packages' TypeScript source from this repository, not their built dist/.
  resolve: { conditions: ["source", ...defaultClientConditions] },
  plugins: [react()],
  build: { target: "es2022", outDir: "../dist/grid/react", emptyOutDir: false },
  esbuild: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
});
