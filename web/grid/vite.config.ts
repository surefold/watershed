import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// The site's grid page. Built into web/dist/grid, beside the playground.
export default defineConfig({
  base: "./",
  plugins: [solid()],
  build: { target: "es2022", outDir: "../dist/grid", emptyOutDir: false },
  esbuild: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
});
