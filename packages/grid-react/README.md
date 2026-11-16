# @surefold/grid-react

The [watershed grid](https://github.com/surefold/watershed/tree/main/packages/grid) in React: a `<Grid>` component, and hooks that
follow a table. The table holds the rows; the component only draws it. Rows
never go through props or component state: change them on the table, and
the grid and any hook that reads it follow.

```tsx
import { useRef } from "react";
import { Table, type GridView } from "@surefold/grid";
import "@surefold/grid/grid.css";
import { Grid, useTable, useTotal } from "@surefold/grid-react";

function Portfolio() {
  const positions = useTable(() => {
    const t = new Table({
      fields: { name: "text", shares: "number", cost: "number", price: "number" },
      formulas: { gain: "shares * (price - cost)" },
      totals: { gain: "sum" },
    });
    t.put("P-1", { name: "Arbor", shares: 100, cost: 50, price: 52 });
    return t;
  });
  const gain = useTotal(positions, "gain");
  const view = useRef<GridView>(null);

  return (
    <>
      <p>Gain {gain}</p>
      <button onClick={() => positions.set("P-1", "price", 55)}>Move the price</button>
      <Grid ref={view} table={positions} theme="paper" columns={{ gain: { signed: true } }} style={{ height: "60vh" }} />
    </>
  );
}
```

**[The cookbook](https://surefold.dev/watershed/grid/react/cookbook/)** has whole, running examples in React: a first grid, rows from an API, pages loaded as you scroll, edits saved back to an API, live updates over a WebSocket or SSE, charts that follow the table, and a large table in a worker.

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
npm install @surefold/grid @surefold/grid-react
```

## In a worker

`table` can also be a `WorkerTable`, whose engine runs in a Web Worker (see
[`@surefold/grid`](https://github.com/surefold/watershed/tree/main/packages/grid#in-a-worker)).
The grid and the hooks work the same way: totals and counts come from the
snapshot the worker sends after each recalculation. `useTable` makes a `Table`;
make a `WorkerTable` with `await WorkerTable.create(…)`, for example in an
effect, and pass it in once it is ready.

## Grid

Takes the table and every option of [`drawGrid`](https://github.com/surefold/watershed/tree/main/packages/grid#drawgrid), plus
`className` and `style`. The ref is the view, for its methods (grouping,
looking back, explaining the focused cell).

A new `table` draws a new grid, and a new `theme` restyles it. The other
options are read when the grid is drawn. Handlers such as `onFocus` are read
when they are called, so a new one is used without drawing again.

## Hooks

- `useTable(() => new Table(…))`: a table made once for the component and
  freed when it unmounts (strict mode's unmount and mount again is allowed
  for). A table made outside a component lives as long as the page.
- `useTotal(table, column)`: a column's total.
- `useReading(table, (t) => …)`: anything read from the table. Return a value
  that compares with `===`, such as a number or a string.
- `useChange(table)`: what the last recalculation did.

The hooks read with `table.peek`, so a render never sets off a
recalculation. They render again after each one.

[`web/grid-react`](https://github.com/surefold/watershed/tree/main/web/grid-react) is an example built only from this
and `@surefold/grid`.
