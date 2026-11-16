import { resolve } from "node:path";
import { defaultClientConditions, defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import solid from "vite-plugin-solid";

// The pages the tests drive, built into web/dist/e2e beside the grid's pages
// so that one server serves both. They are not part of the site: web/build.sh
// starts web/dist clean.
//
// The unmount fixtures take cookbook recipes in both frameworks, so Solid's
// JSX is compiled for the Solid files and React's for the rest.
const SOLID = [/\/web\/grid\/src\/.*\.tsx$/, /\/packages\/grid-solid\/src\/.*\.tsx$/, /\/fixtures\/unmount-solid\.tsx$/];

export default defineConfig({
  root: resolve(__dirname, "fixtures"),
  base: "./",
  // Use the packages' TypeScript source from this repository, not their built dist/.
  resolve: { conditions: ["source", ...defaultClientConditions] },
  plugins: [solid({ include: SOLID }), react({ exclude: SOLID })],
  // The table's worker is a module, with a top-level await for its engine.
  worker: { format: "es" },
  build: {
    target: "es2022",
    outDir: resolve(__dirname, "../dist/e2e"),
    emptyOutDir: true,
    rollupOptions: {
      input: {
        onedit: resolve(__dirname, "fixtures/onedit.html"),
        "unmount-solid": resolve(__dirname, "fixtures/unmount-solid.html"),
        "unmount-react": resolve(__dirname, "fixtures/unmount-react.html"),
      },
    },
  },
  esbuild: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
});
