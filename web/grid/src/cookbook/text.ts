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
      "If <code>loadMore</code> throws, the grid passes the message to <code>onError</code>, calls <code>onStatus</code> with <code>{ failed: true, error }</code>, and asks again on the next scroll near the end, or when you call <code>fetchMore()</code>. A list already at its end cannot scroll further, so a person there needs a way to ask: here the status says the page did not load, and a Try again button calls <code>fetchMore()</code>.",
      "Sorting, filtering and grouping work over the rows loaded so far, and totals are of those rows.",
    ],
  },
  {
    id: "save",
    title: "Edit and save back to an API",
    intro: [
      "People edit in the grid itself: double-click a cell or press Enter, paste from a spreadsheet, fill a block with ⌘Enter, and undo with ⌘Z. " +
        "The grid's <code>onEdit</code> says what they changed: each cell, with what it held before and after, and whether it was typed, pasted, filled, undone or deleted. " +
        "Changes made from code (<code>put</code>, <code>set</code>, loading, discarding) are not reported, so what <code>onEdit</code> hears is what needs saving.",
      "Keep what the server last had, so an edit undone or typed back to it stops counting. Saving sends one <code>PATCH</code> a changed row, with only its changed fields. Edit some titles, prices or stock, then save. DummyJSON answers as if it had saved, but keeps nothing.",
    ],
    notes: [
      "<code>onEdit</code> gets <code>{ cause, cells, removed }</code>: <code>cause</code> is <code>\"edit\"</code>, <code>\"paste\"</code>, <code>\"fill\"</code>, <code>\"undo\"</code>, <code>\"redo\"</code> or <code>\"delete\"</code>; each cell is <code>{ key, column, before, after }</code>; <code>removed</code> is the keys of deleted rows.",
      "It costs only what was edited, so it suits a table of any size, in the page or in a worker. With a <code>WorkerTable</code> it is called once the worker has made the change.",
      "A row that fails to save stays unsaved, so pressing Save again retries it. A field changed again while a save is on its way also stays unsaved, because only what was sent is marked as saved.",
      "To save as people type, call the same <code>save</code> from <code>onEdit</code>, after a pause of a second or so with no edits.",
      "Deleted rows come in <code>removed</code>, for a <code>DELETE</code> each; new rows are ones your code puts, so it knows to <code>POST</code> them.",
      "The grid's <code>rowClass</code> gives a row classes by its key: here <code>unsaved</code>, tinted in the page's CSS. It is asked each time a row is drawn, so the tint follows the row as you scroll or sort; when the unsaved rows change, <code>view.redrawRows()</code> draws the rows in view again.",
    ],
  },
  {
    id: "websocket",
    title: "Prices over a WebSocket",
    intro: [
      "A stream sends many small changes. Each message only needs to <code>set</code> the cell it names, and the table recalculates what that cell feeds. " +
        "Messages can come faster than the screen draws, so gather them as they come and write them once a frame, inside <code>batch</code>: one recalculation, and one redraw, however many came.",
      "This one subscribes to <a href=\"https://docs.cdp.coinbase.com/exchange/websocket-feed/overview\">Coinbase's public ticker</a> (no key needed). Each coin's row is added with its first price, and its gain is worked out from there. Press Connect.",
    ],
    notes: [
      "Only the latest price of each coin is kept between frames, so a burst of messages costs one <code>set</code> a coin.",
      "With <code>history</code> on the table, a column's <code>trend</code> draws how each cell moved, here the last 120 recalculations of each price.",
      "Close the socket when the component goes. A real app would also reconnect after a dropped connection, waiting a little longer each time.",
    ],
  },
  {
    id: "sse",
    title: "Events over SSE",
    intro: [
      "Server-sent events come through the browser's <code>EventSource</code>, a message at a time. Here each message is a new row: they are gathered and <code>put</code> once a frame, in a <code>batch</code> that also <code>drop</code>s the oldest rows, so the table keeps the latest 2,000.",
      "This one reads <a href=\"https://stream.wikimedia.org/?doc\">Wikimedia's stream of recent changes</a>: every edit to every Wikimedia wiki, as it happens. The rows are grouped by wiki, so each group's row counts its edits and adds up the bytes they changed. Press Connect.",
    ],
    notes: [
      "<code>EventSource</code> reconnects by itself after a dropped connection; <code>onerror</code> says when it is trying.",
      "The grouping, sorting and filters are the table's, so they hold while rows come and go. Open a group, or sort by bytes, while it streams.",
      "Browsers stop calling <code>requestAnimationFrame</code> in a hidden tab, so edits gather until the tab is shown again; only the latest 2,000 of them are written.",
    ],
  },
  {
    id: "effect",
    title: "Streams and retries with Effect",
    intro: [
      "The WebSocket recipe again, with <a href=\"https://effect.website\">Effect</a> doing the parts a real app needs and the hand-written one leaves out. " +
        "The connection is Effect's own WebSocket, <code>Socket.makeWebSocket</code>, read as a <code>Stream</code> of messages: the socket is opened when the stream starts and closed however it stops, and the subscription is sent each time it connects. " +
        "When the server drops it, <code>Stream.retry</code> opens another on a <code>Schedule</code>: an exponential backoff, capped, with jitter, which starts again once messages come. " +
        "Batching is a step of the stream, <code>groupedWithin</code>, in place of the frame timer, and each batch is one <code>table.batch</code>.",
      "Connect runs it all as a fiber. Disconnect, or the component going, interrupts the fiber: the socket closes, or the wait to reconnect stops, and nothing is left running. The table and the grid are the same as before. Press Connect.",
    ],
    notes: [
      "<code>npm install effect</code>. This page uses Effect 4. The grid packages do not depend on it: Effect lives in your app, and the table only sees <code>batch</code>, <code>put</code> and <code>set</code>.",
      "It costs some bundle. The parts of Effect this recipe uses come to about 110 KB minified, 38 KB gzipped, half of it the runtime and streams and half <code>Socket</code> (which brings in Effect's channels and layers); with the recipe and its source, this page's JavaScript went from 55 KB to 96 KB gzipped (Solid), and from 118 KB to 159 KB (React). The runtime is shared with any other Effect in the app.",
      "The schedule's <code>tap</code> sees each wait before it starts, with its length and the attempt: that is where the status line comes from. To give up after ten failed tries in a row, add <code>Schedule.upTo({ times: 10 })</code> to the schedule's <code>pipe</code>; the fiber then ends with the last error.",
      "<code>Socket</code> is in <code>effect/socket</code>, which Effect marks unstable: its API may change in a minor release, so pin Effect's version.",
      "The same shape fits SSE or polling. Effect has no <code>EventSource</code> of its own, so for SSE make the stream with <code>Stream.callback</code>: open the <code>EventSource</code> with <code>Effect.acquireRelease</code> (so it is closed however the stream stops), offer its messages to the queue, and fail on its <code>error</code>, so that your schedule decides when to try again rather than the browser. " +
        "For polling, the stream is <code>Stream.fromEffectSchedule(load, Schedule.spaced(\"5 seconds\"))</code>, where <code>load</code> is an <code>Effect.tryPromise((signal) =&gt; fetch(url, { signal }))</code>: interrupting the fiber aborts the request, and the same <code>retry</code> backs off while the server is down.",
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
      "<code>useGroups</code> (React) and <code>createGroups</code> (Solid) follow <code>groups()</code>, and change only when a group, its row count or a total does. For other lists or objects, give <code>useReading</code> or <code>createReading</code> a third argument, <code>isEqual(a, b)</code>, so a value that compares equal is kept rather than drawn again.",
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
      "Writes (<code>put</code>, <code>putMany</code>, <code>set</code>, <code>batch</code>) are messages to the worker, run in order. Each returns a promise, which you can ignore or wait for. It resolves once the worker has recalculated and the page has the snapshot that includes the write, so after <code>await table.putMany(…)</code>, <code>size</code> and <code>total</code> count those rows. To follow totals as they change, whoever changed them, read them in <code>watch</code>, <code>createTotal</code> or <code>useTotal</code>.",
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
