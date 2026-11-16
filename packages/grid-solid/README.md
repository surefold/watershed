# @surefold/grid-solid

The [watershed grid](https://github.com/surefold/watershed/tree/main/packages/grid) in Solid: a `<Grid>` component, and signals that
follow a table. The table holds the rows; the component only draws it.

```tsx
import { Table } from "@surefold/grid";
import "@surefold/grid/grid.css";
import { Grid, createTotal } from "@surefold/grid-solid";

const trades = new Table({
  fields: { symbol: "text", qty: "number", price: "number", mark: "number" },
  formulas: { pnl: "qty * (mark - price)" },
  totals: { qty: "sum", pnl: "sum" },
});

function Book() {
  const pnl = createTotal(trades, "pnl");
  return (
    <>
      <p>P&L {pnl()}</p>
      <Grid table={trades} theme="dark" keyColumn={{ title: "trade" }} columns={{ pnl: { signed: true } }} style={{ height: "60vh" }} />
    </>
  );
}

trades.put("T-1", { symbol: "OAKS", qty: 500, price: 101.2, mark: 101.5 });
trades.set("T-1", "mark", 102);   // the grid and pnl() follow
```

**[The cookbook](https://surefold.dev/watershed/grid/cookbook/)** has whole, running examples in Solid: a first grid, rows from an API, pages loaded as you scroll, edits saved back to an API, live updates over a WebSocket or SSE, charts that follow the table, and a large table in a worker.

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
npm install @surefold/grid @surefold/grid-solid
```

## In a worker

`table` can also be a `WorkerTable`, whose engine runs in a Web Worker (see
[`@surefold/grid`](https://github.com/surefold/watershed/tree/main/packages/grid#in-a-worker)).
The grid and the signals work the same way: totals and counts come from the
snapshot the worker sends after each recalculation.

## Grid

Takes the table and every option of [`drawGrid`](https://github.com/surefold/watershed/tree/main/packages/grid#drawgrid), plus
`class`, `style` and `ref`. `ref` is called with the view, for its methods
(grouping, looking back, explaining the focused cell).

A new `table` draws a new grid, and a new `theme` restyles it. The other
options are read when the grid is drawn. Handlers such as `onFocus` are read
when they are called, so a new one is used without drawing again.

## Signals

- `createTotal(table, column)`: a column's total.
- `createReading(table, (t) => …, isEqual?)`: anything read from the table,
  such as `(t) => t.get("T-1")?.pnl`. It is set after every recalculation;
  with `isEqual`, only when the new value compares unequal to the last.
- `createGroups(table)`: the table's `groups()`, set only when a group, its
  row count or a total changed.
- `createChange(table)`: what the last recalculation did.

Each follows the table from its first recalculation after it is made, and
stops when its owner is cleaned up.

The site's grid page, [`web/grid`](https://github.com/surefold/watershed/tree/main/web/grid), is built with this.
