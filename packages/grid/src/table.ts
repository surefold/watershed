// A table of keyed rows whose formulas and totals are worked out by the
// watershed engine, in WebAssembly. Changing a cell recalculates only the
// cells and totals that depend on it.
//
//   const trades = new Table({
//     fields: { qty: "number", price: "number", mark: "number", side: "text" },
//     formulas: { pnl: "qty * (mark - price)" },
//     totals: { qty: "sum", pnl: "sum" },
//   });
//   trades.put("T-1", { qty: 500, price: 101.2, mark: 101.5, side: "buy" });
//   trades.set("T-1", "mark", 102);
//   trades.total("pnl");   // 400
//
// Writes are gathered up and recalculated once, at the end of the current
// task; reads (`get`, `total`, `why`) recalculate first if anything is waiting,
// so they always see what was written. Two exceptions: inside `batch`, and
// inside `peek`, reads see the last recalculation.

import init, { Grid } from "../pkg/watershed_grid.js";
import { viewStateOf } from "./table-view.js";

await init();

export type Kind = "number" | "text" | "bool";
export type Total = "sum" | "avg" | "min" | "max" | "count";

/** What a value of each kind of field is in JavaScript. */
export type ValueOf<K extends Kind> = K extends "number" ? number : K extends "text" ? string : boolean;

/** A cell as the engine gives it back: a calculated one can be an error. */
export type Cell = number | string | boolean | null | { error: string };

export type Fields = Record<string, Kind>;

/** A table of any columns, for code that handles whatever table it is given. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type AnyTable = Table<any, any>;

/** A table in the page or in a worker: what a grid can draw. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type AnyGridTable = Table<any, any> | import("./remote.js").WorkerTable<any, any>;

/** A row to put: a value for every field. */
export type RowOf<F extends Fields> = { [N in keyof F]: ValueOf<F[N]> };

/**
 * Many rows' fields, a column at a time, for `putMany`: for each field, its
 * value in every row. A number column can be a `Float64Array`.
 */
export type ColumnsOf<F extends Fields> = { [N in keyof F]: ArrayLike<ValueOf<F[N]>> };

export interface TableSpec<F extends Fields, C extends string> {
  /** The columns you give values for, and their kinds, in order. */
  fields: F;
  /** Calculated columns, after the fields, such as `{ pnl: "qty * (mark - price)" }`. */
  formulas?: { [N in C]: string };
  /** Named numbers formulas can use, changed with `setValue`. */
  values?: Record<string, number>;
  /** What each column's total is, in the bottom row and in each group's row. */
  totals?: Partial<Record<Extract<keyof F, string> | NoInfer<C>, Total>>;
  /** Keep values from this many recalculations back, for trends and looking back. */
  history?: number;
}

export interface Column {
  name: string;
  kind: Kind;
  /** The formula, for a calculated column. */
  formula: string | null;
  total: Total | "none";
  /** Text columns: rows can be grouped by them. */
  groupable: boolean;
}

export interface Group {
  key: string;
  rows: number;
  /** Each column's total over the group's rows, by column name. */
  totals: Record<string, number | null>;
}

/** How the rows are shaped for showing: by column name. */
export interface ViewState {
  /** The columns sorted by, the first mattering most. */
  sort: { column: string; desc: boolean }[];
  /** Each filtered column's filter, as typed (`>100`, `5..10`, `=Rates`, text to find). */
  filters: Record<string, string>;
  /** Text every row shown has in its key or a text cell. */
  search: string;
  /** The column rows are grouped by, if any. */
  group: string | null;
}

/** What one recalculation did. */
export interface Change {
  /** Counts recalculations, from 1. */
  epoch: number;
  inputsChanged: number;
  /** How many nodes were worked out again. */
  recalculated: number;
  /** How many of those came out different. */
  changed: number;
  /** How many nodes the table has. */
  nodes: number;
  /** The view's rows were filtered and sorted again. */
  rebuilt: boolean;
  /** Only the rows that changed were moved in the view. */
  patched: boolean;
  /** A column was added, or its total changed. */
  columns: boolean;
  /** The sort, a filter, the search or the grouping changed (see `viewState`). */
  view: boolean;
  /** `performance.now()` when the recalculation started. */
  started: number;
  /** How long it took, in milliseconds. */
  ms: number;
}

type Engine = Grid;
type Name<F extends Fields, C extends string> = Extract<keyof F, string> | C;


function readCell(cell: unknown): Cell {
  if (cell !== null && typeof cell === "object") return { error: String((cell as { e: unknown }).e) };
  return cell as Cell;
}

export class Table<F extends Fields = Fields, C extends string = string> {
  /** The WebAssembly grid underneath. Not part of the API: for this package's views. */
  readonly engine: Engine;
  #fields: string[] = [];
  #kinds: Kind[] = [];
  #names: string[] = [];
  #batching = 0;
  #peeking = 0;
  #position = new Map<string, number>();
  #watchers = new Set<(change: Change) => void>();
  #waiting = false;
  #scheduled = false;
  #columnsChanged = true;
  #viewChanged = false;
  #disposed = false;

  constructor(spec: TableSpec<F, C>);
  /** @internal Wraps an engine that already has its columns (as the demo does). */
  constructor(spec: null, engine: Engine);
  constructor(spec: TableSpec<F, C> | null, engine?: Engine) {
    this.engine = engine ?? new Grid();
    if (spec) {
      if (spec.history) this.engine.keep_history(spec.history);
      const totals: Record<string, string | undefined> = spec.totals ?? {};
      for (const [name, value] of Object.entries(spec.values ?? {})) this.engine.add_value(name, value);
      for (const [name, kind] of Object.entries(spec.fields)) this.engine.add_field(name, kind, totals[name] ?? "none");
      for (const [name, formula] of Object.entries<string>(spec.formulas ?? ({} as Record<string, string>))) {
        this.engine.add_formula(name, formula, totals[name] ?? "none");
      }
    }
    this.#readColumns();
  }

  // ------------------------------------------------------------------ writes

  /** Adds a row, or replaces the row with this key. */
  put(key: string, row: RowOf<F>): void {
    const values = row as Record<string, unknown>;
    this.engine.upsert(
      key,
      this.#fields.map((name) => {
        const value = values[name];
        if (value === undefined || value === null) throw new Error(`row ${key} has no ${name}`);
        return String(value);
      }),
    );
    this.#changed();
  }

  /**
   * Makes room for `rows` more rows. Optional, for a large load: arrays then
   * grow once instead of doubling as rows arrive, which briefly needs up to
   * twice the memory. `putMany` makes room for its own rows.
   */
  reserve(rows: number): void {
    this.engine.reserve(rows);
  }

  /**
   * Adds or replaces many rows at once, from columns: `keys[i]` is row i's
   * key, and each field's column holds its value in every row. Much faster
   * than `put` for many rows: numbers go in as numbers (a `Float64Array`
   * is used as it is), and each distinct text goes in once, so rows that
   * hold the same text share it. Everything is checked first: a bad value
   * adds no rows.
   *
   *   trades.putMany(["T-1", "T-2"], { qty: [500, 200], side: ["Buy", "Sell"], … });
   */
  putMany(keys: readonly string[], columns: ColumnsOf<F>): void {
    const rows = keys.length;
    const kinds = this.#kinds;
    const count = (kind: Kind) => kinds.filter((k) => k === kind).length;
    const numbers = new Float64Array(rows * count("number"));
    const bools = new Uint8Array(rows * count("bool"));
    const codes = new Uint32Array(rows * count("text"));
    const texts: string[] = [];
    const starts: number[] = [0];
    const given = columns as Record<string, ArrayLike<unknown> | undefined>;
    let [n, b, t] = [0, 0, 0];
    this.#fields.forEach((name, k) => {
      const column = given[name];
      if (!column || column.length !== rows) throw new Error(`${name} needs ${rows} values, one for each key`);
      switch (kinds[k]) {
        case "number":
          numbers.set(column as ArrayLike<number>, n++ * rows);
          break;
        case "bool":
          for (let r = 0; r < rows; r++) bools[b * rows + r] = column[r] ? 1 : 0;
          b++;
          break;
        case "text": {
          // Each distinct text once, and for each row which one it is.
          const seen = new Map<string, number>();
          for (let r = 0; r < rows; r++) {
            const text = String(column[r]);
            let code = seen.get(text);
            if (code === undefined) {
              code = seen.size;
              seen.set(text, code);
              texts.push(text);
            }
            codes[t * rows + r] = code;
          }
          starts.push(texts.length);
          t++;
          break;
        }
      }
    });
    this.engine.upsert_many(keys as string[], numbers, bools, codes, texts, Uint32Array.from(starts));
    this.#changed();
  }

  /** Changes one field of a row. */
  set<N extends Extract<keyof F, string>>(key: string, field: N, value: ValueOf<F[N]>): void {
    this.engine.set_cell(key, this.#col(field), String(value));
    this.#changed();
  }

  /** Changes some fields of a row. */
  patch(key: string, values: Partial<RowOf<F>>): void {
    for (const [name, value] of Object.entries(values)) {
      if (value !== undefined) this.engine.set_cell(key, this.#col(name), String(value));
    }
    this.#changed();
  }

  /** Removes a row. Says whether there was one. */
  drop(key: string): boolean {
    const had = this.engine.remove(key);
    if (had) this.#changed();
    return had;
  }

  /** Changes a named number from `values`; every formula that uses it follows. */
  setValue(name: string, value: number): void {
    this.engine.set_value(name, value);
    this.#changed();
  }

  /** Adds a calculated column, such as `bps = pnl / notional * 10000`. */
  addFormula(name: string, formula: string, total: Total | "none" = "none"): void {
    this.engine.add_formula(name, formula, total);
    this.columnsChanged();
  }

  /**
   * Runs `edits` and recalculates once, straight after. The changes are one
   * step for undo in a view. Reads inside it see the table as it was before
   * the batch: read what you need first, then write.
   */
  batch(edits: () => void): Change | undefined {
    const outermost = this.#batching === 0;
    if (outermost) this.engine.begin_edits();
    this.#batching++;
    try {
      edits();
    } finally {
      this.#batching--;
      if (outermost) this.engine.end_edits();
    }
    // A batch inside a batch leaves the recalculation to the outer one.
    return outermost ? this.flush() : undefined;
  }

  /**
   * Runs `read` against the table as of its last recalculation, without
   * recalculating first, even if writes are waiting. For reading while
   * something else is drawing, such as a React render.
   */
  peek<T>(read: (table: this) => T): T {
    this.#peeking++;
    try {
      return read(this);
    } finally {
      this.#peeking--;
    }
  }

  // ------------------------------------------------------------- the view

  /**
   * Sorts the rows by these columns, the first mattering most; `[]` for the
   * order they came in. A grid drawing the table shows it.
   */
  sort(by: { column: Name<F, C>; desc?: boolean }[]): void {
    this.engine.set_sort(by.map((s) => `${this.#col(s.column)}:${s.desc ? "desc" : "asc"}`).join(","));
    this.#shaped();
  }

  /** Filters a column: `>100`, `5..10`, `=Rates`, `=Rates|FX`, or text to find; `""` for none. */
  filter(column: Name<F, C>, text: string): void {
    this.engine.set_filter(this.#col(column), text);
    this.#shaped();
  }

  /** Shows only rows whose key or a text cell has this text; `""` for all. */
  search(text: string): void {
    this.engine.set_quick_filter(text);
    this.#shaped();
  }

  /** Groups the rows by a text column, or not at all for `null`. */
  group(column: Name<F, C> | null): void {
    this.engine.set_group(column === null ? -1 : this.#col(column));
    this.#shaped();
  }

  /** How the rows are shaped now: sort, filters, search and grouping. */
  viewState(): ViewState {
    return viewStateOf(this.engine.view_state(), this.#names);
  }

  #shaped(): void {
    this.#viewChanged = true;
    this.#changed();
  }

  // ------------------------------------------------------------------- reads

  /** A row's cells by column name, or `undefined` if there is no such row. */
  get(key: string): (RowOf<F> & Record<C, Cell>) | undefined {
    this.#settle();
    const json = this.engine.row(key);
    if (json === undefined) return undefined;
    const cells = JSON.parse(json) as unknown[];
    return Object.fromEntries(this.#names.map((name, k) => [name, readCell(cells[k])])) as RowOf<F> & Record<C, Cell>;
  }

  /** A column's total over the rows that pass the filters, or `null` if it has none. */
  total(column: Extract<keyof F, string> | C): number | null {
    this.#settle();
    const totals = JSON.parse(this.engine.totals()) as (number | null)[];
    return totals[this.#col(column)] ?? null;
  }

  /** Why a cell has its value: what it was worked out from, as an indented tree. */
  why(key: string, column: Extract<keyof F, string> | C): string {
    this.#settle();
    return this.engine.explain(key, this.#col(column));
  }

  /** Why a column's total has its value. */
  whyTotal(column: Extract<keyof F, string> | C): string {
    this.#settle();
    return this.engine.explain_total(this.#col(column));
  }

  /** When rows are grouped (in a view): each group, its row count and its totals, in the view's order. */
  groups(): Group[] {
    this.#settle();
    const names = this.#names;
    return (JSON.parse(this.engine.groups()) as { g: string; n: number; c: (number | null)[] }[]).map((g) => ({
      key: g.g,
      rows: g.n,
      totals: Object.fromEntries(names.map((name, k) => [name, g.c[k] ?? null])),
    }));
  }

  /** How a column's total moved over recent recalculations, oldest first (needs `history`). */
  totalTrend(column: Extract<keyof F, string> | C, points = 120): number[] {
    this.#settle();
    return (JSON.parse(this.engine.total_trend(this.#col(column), points)) as number[] | null) ?? [];
  }

  /** The oldest and latest recalculations a view can look back to, or `null` (needs `history`). */
  history(): [oldest: number, latest: number] | null {
    return JSON.parse(this.engine.history_range()) as [number, number] | null;
  }

  /** How many rows there are. */
  get size(): number {
    return this.engine.row_count();
  }

  /** The columns, fields first, then formulas. */
  columns(): Column[] {
    return (JSON.parse(this.engine.columns()) as { name: string; kind: Kind; formula: string | null; agg: Total | "none"; groupable: boolean }[]).map(
      (c) => ({ name: c.name, kind: c.kind, formula: c.formula, total: c.agg, groupable: c.groupable }),
    );
  }

  /** The rows that pass the filters, in the view's order, as CSV. */
  csv(): string {
    this.#settle();
    return this.engine.csv();
  }

  /** Recalculates every node from scratch and compares: `null` if all match, or what differs. */
  check(): string | null {
    this.#settle();
    return this.engine.check() ?? null;
  }

  // ------------------------------------------------------------- recalculating

  /** Calls `watcher` after every recalculation. Returns a function that stops it. */
  watch(watcher: (change: Change) => void): () => void {
    this.#watchers.add(watcher);
    return () => this.#watchers.delete(watcher);
  }

  /** Recalculates now, instead of at the end of the current task. */
  flush(): Change {
    this.#waiting = false;
    const started = performance.now();
    const report = JSON.parse(this.engine.update()) as Omit<Change, "columns" | "started" | "ms">;
    const change: Change = {
      ...report,
      columns: this.#columnsChanged,
      view: this.#viewChanged,
      started,
      ms: performance.now() - started,
    };
    this.#columnsChanged = false;
    this.#viewChanged = false;
    for (const watcher of [...this.#watchers]) watcher(change);
    return change;
  }

  /** @internal The columns changed in the engine (a view changed a total): read them again. */
  columnsChanged(): void {
    this.#readColumns();
    this.#columnsChanged = true;
    this.#changed();
  }

  /** @internal The engine was changed directly (as the demo does): recalculate soon. */
  touched(): void {
    this.#changed();
  }

  /** Frees the engine's memory. The table can't be used after. */
  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#watchers.clear();
    this.engine.free();
  }

  [Symbol.dispose](): void {
    this.dispose();
  }

  // ----------------------------------------------------------------- private

  #readColumns(): void {
    const columns = this.columns();
    this.#position = new Map(columns.map((c, k) => [c.name, k]));
    this.#names = columns.map((c) => c.name);
    const fields = columns.filter((c) => c.formula === null);
    this.#fields = fields.map((c) => c.name);
    this.#kinds = fields.map((c) => c.kind);
  }

  #col(name: string): number {
    const col = this.#position.get(name);
    if (col === undefined) throw new Error(`there is no column called ${name}`);
    return col;
  }

  #changed(): void {
    this.#waiting = true;
    if (this.#scheduled) return;
    this.#scheduled = true;
    queueMicrotask(() => {
      this.#scheduled = false;
      if (this.#waiting && !this.#disposed) this.flush();
    });
  }

  /** Recalculates first if writes are waiting, unless inside a batch or a peek. */
  #settle(): void {
    if (this.#waiting && this.#batching === 0 && this.#peeking === 0) this.flush();
  }
}
