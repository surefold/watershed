# @surefold/grid

A data grid whose rows, formulas and totals are worked out by the watershed
engine, in WebAssembly. Changing a cell recalculates only the cells and
totals that depend on it. No framework: [`@surefold/grid-solid`](https://github.com/surefold/watershed/tree/main/packages/grid-solid)
and [`@surefold/grid-react`](https://github.com/surefold/watershed/tree/main/packages/grid-react) put it in a component.

It has two parts. A **`Table`** holds the rows and works them out. **`drawGrid`**
draws a table and redraws it whenever it is recalculated. Data goes in and
out through the table; the grid only draws.

```ts
import { Table, drawGrid } from "@surefold/grid";
import "@surefold/grid/grid.css";

const orders = new Table({
  fields: { region: "text", qty: "number", price: "number" },
  formulas: { line: "qty * price * (1 + tax)" },
  values: { tax: 0.2 },
  totals: { qty: "sum", line: "sum" },
});
orders.put("o-1", { region: "EU", qty: 2, price: 10 });

drawGrid(document.querySelector("#grid")!, orders, {
  keyColumn: { title: "order" },
  columns: { qty: { decimals: 0 } },
});

orders.set("o-1", "qty", 5);   // o-1's line and two totals are recalculated; the grid redraws
orders.total("line");           // 60
orders.why("o-1", "line");      // what line was worked out from
```

## Install

The packages are on GitHub Packages. Tell npm where `@surefold` packages
come from, with a GitHub token that can read packages (a classic token with
`read:packages`; GitHub asks for one even for public packages), in a
`.npmrc` beside your `package.json`:

```
@surefold:registry=https://npm.pkg.github.com
//npm.pkg.github.com/:_authToken=${GITHUB_TOKEN}
```

Then:

```bash
npm install @surefold/grid
```

## Table

`new Table(spec)`, where the spec has:

| Key | What it is |
|---|---|
| `fields` | The columns you give values for, and their kinds: `"number"`, `"text"` or `"bool"`. |
| `formulas` | Calculated columns, by name, such as `"qty * (mark - price)"` or `if(side == "Buy", 1, -1)`. |
| `values` | Named numbers formulas can use, changed later with `setValue`. |
| `totals` | Each column's total: `"sum"`, `"avg"`, `"min"`, `"max"` or `"count"`. |
| `history` | Keep this many recalculations back, for trends in cells and for looking back. |

**Writing:** `put(key, row)` adds or replaces a row and needs every field.
`putMany(keys, columns)` adds or replaces many rows from columns (`{ qty:
Float64Array, side: string[], … }`), about twice as fast as `put` for a large
load, with each distinct text stored once; a bad value adds no rows.
`reserve(rows)` makes room before a large load, so arrays grow once instead
of doubling, which briefly needs twice the memory.
`set(key, field, value)` changes one field, `patch(key, values)` some of them,
`drop(key)` removes a row. `setValue(name, n)` changes a named number, and
`addFormula(name, formula, total?)` adds a column while it runs.

**Reading:** `get(key)` is a row by column name, `total(column)` a column's
total, `why(key, column)` and `whyTotal(column)` say what a value was worked
out from. Also `size`, `columns()`, `groups()`, `totalTrend(column)`,
`history()`, `csv()`, and `check()`, which compares every node with a full
recalculation.

**Shaping the rows:** `sort([{ column, desc }])`, `filter(column, text)`
(`>100`, `5..10`, `=Rates`, `=Rates|FX`, or text to find), `search(text)`
and `group(column | null)` decide which rows show and in what order, and
`viewState()` says how they are set now. They are the table's, not a
grid's: a grid drawing the table shows what is set here, and what someone
sets from the grid's headers and filter boxes shows here. In a `WorkerTable`
they run in the worker.

**When it recalculates.** Writes are gathered up and recalculated once, at the
end of the current task. Reads recalculate first if writes are waiting, so
they always see what was written, with two exceptions:

- `batch(() => …)` recalculates once, straight after its edits, and makes them
  one step for undo. Reads inside it see the table as it was before the batch,
  so read what you need first, then write.
- `peek((t) => …)` reads as of the last recalculation without starting one,
  for reading while something else is drawing.

`watch(fn)` calls `fn` after every recalculation with what it did (inputs
changed, nodes recalculated, how long it took) and returns a function that
stops it. `dispose()` frees the engine's memory at once; otherwise it is freed
when the table is garbage collected.

Names are checked by TypeScript: `orders.set("o-1", "qyt", 5)` does not
compile, and `put` needs every field, of its kind. Code that takes any table
uses the `AnyTable` type.

## drawGrid

`drawGrid(element, table, options)` draws into an element that has a height,
and returns the view. Options:

| Option | What it does |
|---|---|
| `keyColumn` | The first column, which shows each row's key: `{ title, width }`. |
| `columns` | How each column looks, by name: `decimals`, `signed` (colour by sign), `width`, and `trend` (how the cell moved; needs `history`). |
| `theme` | One of `themes` (light, dark, paper, dusk, terminal, high contrast), or a `[data-wg-theme="name"]` rule of your own. With none, it takes the page's colours. |
| `loadMore` | `async () => boolean`, called as the person nears the end: add rows to the table, and resolve to whether there may be more. |
| `onStatus`, `onFocus`, `onGroup`, `onError`, `onNote` | What happened: each recalculation and its cost, the cell the person is on, grouping, and messages to show them. |

The view's methods group (`setGroup`, `expandAll`, `revealGroup`), sort and
filter (`setSort`, `setFilter`, `setQuickFilter`, the same as the table's
`group`, `sort`, `filter` and `search`, by column position), fit columns to what
they show (`fitColumn`, `fitColumns`; people double-click a column's edge),
look back
(`setAsOf(epoch)`), explain the focused cell (`explainFocus`), keep and restore
the column layout (`getLayout`, `setLayout`), and `destroy()` it.

People can sort, filter, group, pin, hide, move and resize columns, edit,
undo, copy and paste to and from a spreadsheet, and pick out a block of cells,
from the grid itself. [The grid's README](https://github.com/surefold/watershed/blob/main/crates/watershed-grid/README.md#what-it-does)
says what each does and how it is worked out.

`GroupChart` draws each group's total as a bar, from `table.groups()`.

## In a worker

`WorkerTable` is a table whose engine runs in a Web Worker. Loading,
recalculating and streaming happen there, so a large load never stops the
page: on 1,000,000 rows loaded with `putMany`, the page's longest wait was
21 ms, where with `Table` it waits for every block and the first
recalculation (seconds).

```ts
import { WorkerTable, drawGrid } from "@surefold/grid";

const trades = await WorkerTable.create({ fields: { … }, formulas: { … }, totals: { … } });
await trades.putMany(keys, columns);
drawGrid(element, trades);                  // the same grid, drawing from the worker
trades.set("T-1", "mark", 102);             // a message; the grid redraws when the worker has recalculated
trades.total("pnl");                        // from the last snapshot, at once
await trades.why("T-1", "pnl");             // answered by the worker
```

It has `Table`'s methods, with two differences:

- **Writes are messages**, run in order, and return promises (which can be
  ignored). `batch(() => …)` sends what it gathers as one message: one
  recalculation, one step for undo.
- **Reads come in two kinds.** `size`, `columns()`, `total()`, `groups()`,
  `totalTrend()` and `history()` read the snapshot the worker sends after
  each recalculation, at once; so do `peek`, `watch` and the Solid and React
  helpers. `get`, `why`, `whyTotal`, `csv` and `check` ask the worker, and
  return promises.

The grid draws a `WorkerTable` as it draws a `Table`: rows in view come in
each snapshot, and rows scrolled to are asked for and drawn when they come.
Copying from a grid in a worker uses the browser's asynchronous clipboard.

The worker is started from this package (`new Worker(new URL(…),
{ type: "module" })`, which Vite, webpack 5 and Rollup bundle). With Vite,
set `worker: { format: "es" }`: the worker is a module. To start it
yourself, pass `{ worker }` to `create`.

## How many rows

A row costs about 2 KB in the browser for a table of 7 fields, 3 formulas
and 6 totals, and a page's WebAssembly memory stops at 4 GB. Loaded with
`putMany` in blocks, such a table holds about 1.5 million rows; with
`reserve(2_000_000)` first, about 2 million. Once loaded, a change still
recalculates only what it reaches: a few price changes take about 0.1 ms
at a million rows. Loading takes about 400,000 rows a second, and the first
recalculation after a large load works out every total once (about half a
second at a million rows in the browser). With a `Table` the page waits for
both; with a `WorkerTable` they happen in the worker.

## Using it

The engine loads when the package is imported (a top-level `await`), and
Vite, webpack 5 and Rollup pick up its `.wasm` file as an asset. Target
ES2022 or later; with Vite, also for dependencies:

```ts
export default defineConfig({
  build: { target: "es2022" },
  optimizeDeps: { esbuildOptions: { target: "es2022" } },
});
```

It runs in the browser only: drawing needs a page, and the engine is loaded
the way a page loads it.

## Publishing

Pushing a tag such as `grid-v0.2.0` publishes all three packages at that
version (`.github/workflows/publish-grid.yml`). First set `version` in each
package's `package.json`, and the dependency on `@surefold/grid` in the
other two, to match.
