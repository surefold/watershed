# watershed-grid

A data grid built on [watershed-core](../watershed-core/README.md) and
compiled to WebAssembly. Rows are a collection, so every cell is a node,
calculated columns are formulas, and totals are aggregates in the graph.
It is used from TypeScript through the packages in
[`packages/`](../../packages/grid/README.md), and the site's grid page is built
with them in [`web/grid/`](../../web/grid).

[`watershed-web`](../watershed-web) is the companion crate: the engine
compiled to WebAssembly behind an interactive page
([`web/demo.html`](../../web/demo.html)) with four preset models, where you
can move inputs, see which nodes recalculate, ask any node to explain its
value, and time a graph of up to a million nodes in the browser.

The page can also run a model as a scenario plan: `Model::scenarios` and
`Model::sweep` compile it with [watershed-gpu](../watershed-gpu/README.md)
and run the plan, for many values of one input, without touching the graph.
There is no CUDA in a browser and no threads in this build, so a plan runs
there on one thread, as updates do.

## Building and running

```bash
cargo install wasm-bindgen-cli --version 0.2.130   # once
rustup target add wasm32-unknown-unknown            # once
# and Node 20 or later, for npm and Vite
web/build.sh
python3 -m http.server -d web/dist 8000
```

- http://localhost:8000 is the interactive page; its "Data grid" tab
  (http://localhost:8000/#grid) shows the grid.
- http://localhost:8000/grid/ is the grid on a page of its own, in Solid.
- http://localhost:8000/grid/react/ is a smaller example in React.

The Rust side is tested natively: `cargo test -p watershed-grid`. The
JavaScript is type-checked with `npm run check` (after `web/build.sh` has
put the engine in `packages/grid/pkg`).

## Using it from JavaScript

Three packages in [`packages/`](../../packages), used by the pages in `web/`.
Each has a README with its whole API:

- [`@surefold/grid`](../../packages/grid/README.md): a `Table` that holds the rows and works out formulas
  and totals, and `drawGrid`, which draws a table. No framework.
- [`@surefold/grid-solid`](../../packages/grid-solid/README.md): `<Grid table={…} />`, and `createTotal` and
  `createReading`, which follow a table as signals.
- [`@surefold/grid-react`](../../packages/grid-react/README.md): `<Grid table={…} />`, and the hooks `useTable`,
  `useTotal`, `useReading` and `useChange`.

The table holds the data and the grid only draws it. Rows never go through
props or component state: change them on the table, and every grid and
total that shows them follows.

```ts
import { Table, drawGrid } from "@surefold/grid";
import "@surefold/grid/grid.css";

const orders = new Table({
  fields: { region: "text", qty: "number", price: "number" },
  formulas: { line: "qty * price" },
  totals: { qty: "sum", line: "sum" },
});
orders.put("o-1", { region: "EU", qty: 2, price: 10 });

const view = drawGrid(document.querySelector("#grid")!, orders, {
  columns: { qty: { decimals: 0 }, line: { signed: true } },
});
orders.set("o-1", "qty", 5);    // recalculates o-1's line and two totals, then the grid redraws
orders.total("line");           // 50
orders.why("o-1", "line");      // what line was worked out from
```

Field and column names are checked by TypeScript: `orders.set("o-1", "qyt", 5)`
does not compile, and `put` needs every field, of its kind.

Writes are gathered up and recalculated once, at the end of the current
task; `table.batch(() => …)` recalculates straight after its edits, and
makes them one step for undo. Reads (`get`, `total`, `why`) recalculate
first if anything is waiting, so they always see what was written, except
inside `batch` and `table.peek(read)`, which see the last recalculation.
The React hooks read with `peek`, so a render never sets one off.

```tsx
// Solid
const pnl = createTotal(trades, "pnl");
<Grid table={trades} theme="dark" keyColumn={{ title: "trade" }} ref={(view) => …} />

// React
const trades = useTable(() => new Table({ … }));
const pnl = useTotal(trades, "pnl");
<Grid table={trades} theme="dark" ref={viewRef} />
```

For large loads, `WorkerTable` runs the same engine in a Web Worker, so the
page never waits on it: see the package's README.

The engine loads when `@surefold/grid` is imported (a top-level `await`),
so a bundler needs to target ES2022 or later. Below the packages is the
WebAssembly `Grid` class itself (`grid.add_field`, `grid.upsert`…), which
is what `Table` wraps.

## What it does

**Cells and formulas.** Calculated columns are formulas typed as text
(`qty * (mark - price)`, `if(side == "Buy", 1, -1)`), parsed by the engine's
[formula parser](../watershed-core/README.md#formulas-as-text). Editing a
cell recalculates that row's formulas and the totals they feed: on 100,000
rows, a handful of nodes out of 1.1 million.

**Totals.** A column's total is a sum, average, minimum, maximum or count,
shown in the bottom row and in each group's row, and can be changed from the
column's menu. While a filter is on, the totals shown are of the rows that
pass it, added up directly; the graph's totals are always of every row.

**Filtering, sorting and the view.** Filtering (`>100`, `5..10`, `=Rates`,
or several values as `=Rates|FX`), sorting by several columns and grouping
are done in Rust. The page asks only for the lines in view and draws those.
The view is left alone unless a change touches a column it reads, and then
only the rows that were set are moved to where they now sort. It is filtered
and sorted from scratch only when that would be quicker (many rows set at
once) or everything may have moved (a named value changed).

**Streaming updates.** In the demo, a stream of 100 price changes per frame
takes about 1 ms to apply and redraw on 100,000 rows, and about 2 ms when
the grid is sorted by a calculated column. These are figures from the demo
page on a desktop browser, not a controlled benchmark.

**History.** With `history: 120` in the table's spec, a column can draw how each cell,
group total and total moved over recent updates beside its number. This uses
the engine's [history](../watershed-core/README.md#history-and-change-tracking),
so the grid stores nothing extra. `view.setAsOf(update)` shows the whole
grid as it was after an earlier update: cells, totals, which rows pass the
filters, and their order. Nothing can be edited until it is back at the
present. "Why this value" uses `explain_at` while looking back.

**Charts.** `GroupChart` ([`packages/grid/src/chart.js`](../../packages/grid/src/chart.js)) draws each
group's total as a bar, read directly from the group nodes.

**Columns and rows.** Columns can be dragged into a new order (dragging onto
the pinned columns pins), pinned to the left, hidden and resized, from each
header and its menu. Double-clicking a column's edge fits its width to its
header, its total and the rows on screen. Rows can arrive as the user scrolls: give the view a
`loadMore` function that adds the next block and returns whether there may
be more. Rows loaded that way are ordinary rows, so sorting, filters and
totals cover everything loaded so far.

**Editing.** Edits can be undone and redone (a paste is one step). Cells
copy and paste as tab-separated text, to and from a spreadsheet. Dragging
over cells, or Shift with the arrow keys, selects a block: it copies, takes
a paste, fills with one value, and the page shows its count, sum, average,
minimum and maximum, kept current as values change.

**Accessibility.** The grid labels its rows, cells and headers for a screen
reader and announces the cell the focus moves to.

**Themes.** `drawGrid(el, table, { theme: "dark" })`, or
`view.setTheme("paper")` later. `themes` lists them (light,
dark, paper, dusk, terminal, high contrast). With none, the grid takes the
page's colours. A theme is one rule in
[`packages/grid/src/grid.css`](../../packages/grid/src/grid.css) that sets the grid's `--wg-`
colours, fonts and corners, so a custom theme is one more rule:
`[data-wg-theme="mine"] { ... }`.

## Grouping

Rows group by any text column: a field, or a formula such as
`if(line >= 100, "big", "small")`, where a row changes group as its cells
change.

Group totals are aggregates in the graph only for small tables. A
[grouping in the graph](../watershed-core/README.md#grouping) costs a
membership node for each row in each totalled column, for every column ever
grouped by, and those nodes are not reclaimed: 50,000 rows with six totals
took about 175 MB per grouping column. Above 100,000 row-and-column pairs
(`set_group_budget`), the grid adds each group's totals up over its rows
instead, which adds nothing to the graph.

## Limitations

- No decimal columns; numbers are `f64`.
- A grid grouped by a formula column recomputes its view after every edit.
  Grouped by a field, only the rows that were set are moved.
- Group totals fall back to direct summation above the group budget, as
  described above.
