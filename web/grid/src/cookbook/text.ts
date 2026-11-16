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
  {
    id: "worker",
    title: "A large table in a worker",
    intro: [
      "A <code>WorkerTable</code> is a table whose engine runs in a Web Worker. Loading and every recalculation happen there, so a large load never stops the page. " +
        "It takes the same spec as a <code>Table</code>, and the grid and the totals draw it the same way. It is made with <code>await WorkerTable.create(spec)</code>, because the worker has to start first.",
      "Press the button to load 300,000 rows in blocks of 50,000. Scroll and sort while they load, then move a thousand prices at once.",
    ],
    notes: [
      "Writes (<code>put</code>, <code>putMany</code>, <code>set</code>, <code>batch</code>) are messages to the worker, run in order. Each returns a promise, which you can ignore or wait for. It resolves once the worker has taken the write; the totals it changes come a moment later, with the recalculation's snapshot, so read them in <code>watch</code>, <code>createTotal</code> or <code>useTotal</code>.",
      "<code>size</code>, <code>total</code>, <code>groups</code> and <code>columns</code> read the snapshot the worker sends after each recalculation, so they answer at once. <code>get</code>, <code>why</code> and <code>csv</code> ask the worker, so they return promises.",
      "With Vite, set <code>worker: { format: \"es\" }</code> in the config: the worker is a module. Webpack 5 and Rollup bundle it as they are.",
      "<code>dispose()</code> stops the worker. A table made in a component should be disposed when the component goes, and promises still waiting on it reject.",
      "A row costs about 2 KB, and a page's WebAssembly memory stops at 4 GB, so a worker holds about 1.5 million rows of a table like this one.",
    ],
  },
];

export const install = {
  npmrc: "@surefold:registry=https://npm.pkg.github.com\n//npm.pkg.github.com/:_authToken=${GITHUB_TOKEN}",
  solid: "npm install @surefold/grid @surefold/grid-solid\n# or\nbun add @surefold/grid @surefold/grid-solid",
  react: "npm install @surefold/grid @surefold/grid-react\n# or\nbun add @surefold/grid @surefold/grid-react",
  /** Bun reads .npmrc too; this is the same, for a project that keeps its settings in bunfig.toml. */
  bunfig: `# bunfig.toml
[install.scopes]
"@surefold" = { url = "https://npm.pkg.github.com", token = "$GITHUB_TOKEN" }`,
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
