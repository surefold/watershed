// A small grid over a Table or a WorkerTable (?table=worker), with what
// onEdit reports kept on `window.fixture` for the tests to read.

import { Table, WorkerTable, drawGrid, type Edits, type GridView } from "@surefold/grid";
import "@surefold/grid/grid.css";

const spec = {
  fields: { name: "text", qty: "number", price: "number" },
  formulas: { line: "qty * price" },
  totals: { qty: "sum", line: "sum" },
} as const;

const ROWS = [
  ["a", { name: "Apple", qty: 1, price: 10 }],
  ["b", { name: "Banana", qty: 2, price: 20 }],
  ["c", { name: "Cherry", qty: 3, price: 30 }],
  ["d", { name: "Date", qty: 4, price: 40 }],
] as const;

const kind = new URLSearchParams(location.search).get("table") === "worker" ? "worker" : "table";
const table = kind === "worker" ? await WorkerTable.create(spec) : new Table(spec);
if (table instanceof WorkerTable) {
  await table.batch(() => {
    for (const [key, row] of ROWS) void table.put(key, row);
  });
} else {
  for (const [key, row] of ROWS) table.put(key, row);
}

const edits: Edits[] = [];
const notes: string[] = [];
const errors: string[] = [];
const view: GridView = drawGrid(document.getElementById("grid")!, table, {
  keyColumn: { title: "key" },
  columns: { qty: { decimals: 0 }, price: { decimals: 0 }, line: { decimals: 0 } },
  // A copy, so a later change to what the grid handed over cannot change what was reported.
  onEdit: (e) => edits.push(structuredClone(e)),
  onNote: (m) => notes.push(m),
  onError: (m) => errors.push(m),
});

declare global {
  interface Window {
    fixture: { kind: string; table: typeof table; view: GridView; edits: Edits[]; notes: string[]; errors: string[] };
  }
}
window.fixture = { kind, table, view, edits, notes, errors };
document.body.dataset.ready = kind;
