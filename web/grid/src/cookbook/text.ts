// What the cookbook says about each recipe, shared by the Solid page
// (web/grid/cookbook) and the React page (web/grid-react/cookbook). Each
// page runs its own framework's version of every recipe and shows its source.

export type Framework = "solid" | "react";

export interface Recipe {
  id: string;
  title: string;
  /** Paragraphs, as HTML: before the live recipe. */
  intro: string[];
  /** Points worth knowing, as HTML: after the source. */
  notes: string[];
}

export const recipes: Recipe[] = [
  {
    id: "first-grid",
    title: "A first grid",
    intro: [
      "A <code>Table</code> holds the rows and works out the formulas and totals. The <code>&lt;Grid&gt;</code> component only draws it. " +
        "So rows never go through props or component state: change them on the table, and the grid and anything that reads a total follow.",
      "Click a cell to edit it, or use the buttons, which change the table from code.",
    ],
    notes: [
      "<code>put</code> needs every field, of its kind, and TypeScript checks the names: <code>set(\"o-1\", \"qyt\", 5)</code> does not compile.",
      "Writes are gathered up and recalculated once, at the end of the current task. Only the cells and totals that depend on what changed are worked out again.",
      "The grid fills the element it is given, so give it a height.",
    ],
  },
  {
    id: "fetch",
    title: "Rows from an API",
    intro: [
      "Fetch, then hand the rows to the table. <code>putMany</code> takes them a column at a time and is the fastest way in; " +
        "<code>put</code> one row at a time works too. Rows are found by key, so fetching again replaces the rows that came before rather than adding copies, " +
        "and only the cells whose values moved are recalculated.",
      "This one reads <a href=\"https://dummyjson.com/docs/products\">DummyJSON's products</a> (a free API, no key needed) and works out each product's sale price and stock value with formulas.",
    ],
    notes: [
      "The API's names do not have to be the table's: map them as you put the rows (<code>discountPercentage</code> becomes <code>discount</code> here).",
      "If rows can disappear between fetches, <code>drop(key)</code> the ones that are gone, inside <code>batch(() =&gt; …)</code> with the new rows so it is one recalculation.",
      "To keep it current, call the same function on a timer, or on each message from a WebSocket: <code>set</code> or <code>patch</code> the rows a message names.",
      "Abort the fetch when the component goes, so a late answer does not write to a table that has been freed.",
    ],
  },
  {
    id: "paged",
    title: "Load pages as you scroll",
    intro: [
      "For an API that pages, give the grid <code>loadMore</code>. It is called as the person nears the last row: put the next page into the table, " +
        "and resolve to whether there may be more. The first page is asked for once the grid is drawn, through the view's <code>fetchMore()</code>.",
      "Scroll down: each page is twenty more products from the same API.",
    ],
    notes: [
      "If <code>loadMore</code> throws, the grid says so through <code>onError</code> and stops asking. Here a failed page is caught and reported, and <code>loadMore</code> resolves to <code>true</code>, so scrolling asks for it again.",
      "Sorting, filtering and grouping work over the rows loaded so far, and totals are of those rows.",
    ],
  },
  {
    id: "charts",
    title: "Charts from the table",
    intro: [
      "A chart reads the table, the way the grid does, and draws again after each recalculation. " +
        "Group the rows (<code>table.group(\"category\")</code>, or from a column's ⋮ menu in the grid) and <code>table.groups()</code> gives each group's row count and totals.",
      "The first chart is the package's own <code>GroupChart</code>. The second is a few lines of SVG drawn from <code>groups()</code>: " +
        "put any charting library in its place. Edit a price or stock in the grid, or press the button, and both follow.",
    ],
    notes: [
      "With <code>history</code> set on the table, <code>totalTrend(column)</code> gives how a total moved over recent recalculations, for a line or sparkline.",
      "<code>groups()</code> follows the grid's filters and group order: filter the grid and the bars are of the rows that pass.",
      "In React, <code>useReading</code> wants a value that compares with <code>===</code>, so a list such as <code>groups()</code> is read as a string and parsed. In Solid, <code>createReading</code> follows any value.",
    ],
  },
];

export const install = {
  npmrc: "@surefold:registry=https://npm.pkg.github.com\n//npm.pkg.github.com/:_authToken=${GITHUB_TOKEN}",
  solid: "npm install @surefold/grid @surefold/grid-solid",
  react: "npm install @surefold/grid @surefold/grid-react",
  vite: `// vite.config.ts
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [/* solid() or react() */],
  worker: { format: "es" },          // only for WorkerTable
  build: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
});`,
  css: `import "@surefold/grid/grid.css";   // once, anywhere in the app`,
};
