// A grid of payments with an exact decimal amount and fee, over a Table or a
// WorkerTable (?table=worker), kept on `window.fixture` for the tests.

import { Table, WorkerTable, drawGrid, type GridView } from "@surefold/grid";
import "@surefold/grid/grid.css";

const spec = {
  fields: { desk: "text", amount: "decimal" },
  formulas: { fee: "amount * 0.015d" },
  totals: { amount: "sum", fee: "sum" },
} as const;

const kind = new URLSearchParams(location.search).get("table") === "worker" ? "worker" : "table";
const table = kind === "worker" ? await WorkerTable.create(spec) : new Table(spec);
// One row put, two in a column: a decimal goes in as text, exactly.
const ready = [
  table.put("p-1", { desk: "Rates", amount: "0.10" }),
  table.putMany(["p-2", "p-3"], { desk: ["Rates", "FX"], amount: ["0.20", "12345678901234567.89"] }),
];
await Promise.all(ready);

const errors: string[] = [];
const view: GridView = drawGrid(document.getElementById("grid")!, table, {
  keyColumn: { title: "key" },
  columns: { fee: { decimals: 4 } },
  onError: (m) => errors.push(m),
});

declare global {
  interface Window {
    decimals: { kind: string; table: typeof table; view: GridView; errors: string[] };
  }
}
window.decimals = { kind, table, view, errors };
document.body.dataset.ready = kind;
