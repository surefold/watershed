// A table whose engine runs in a Web Worker, so loading a million rows or
// recalculating after a large load never stops the page.
//
//   const trades = await WorkerTable.create({ fields: { … }, formulas: { … } });
//   trades.putMany(keys, columns);          // goes to the worker; the page carries on
//   drawGrid(element, trades);               // draws from what the worker sends back
//   await trades.why("T-1", "pnl");
//
// Writes are messages, run in order. Reads the page needs at once (the
// totals, the columns, the rows in view) come from a snapshot the worker
// sends after each recalculation; the rest are answered by the worker, so
// they return promises. The worker answers a write after the snapshot that
// includes it, so once a write resolves, the totals here include it.

import type { Call, FromWorker, Snapshot, ToWorker, Window } from "./protocol.js";
import type { Cell, Change, Column, ColumnsOf, Fields, Group, Kind, RowOf, TableSpec, Total, ValueOf, ViewState } from "./table.js";
import { numberOf, textOf, viewStateOf } from "./table-view.js";

type Name<F extends Fields, C extends string> = Extract<keyof F, string> | C;

interface Pending {
  resolve: (results: unknown[]) => void;
  reject: (error: Error) => void;
}

function readCell(cell: unknown): Cell {
  if (cell !== null && typeof cell === "object") return { error: String((cell as { e: unknown }).e) };
  return cell as Cell;
}

export interface WorkerOptions {
  /** The worker to use. By default one is started from this package's worker module. */
  worker?: Worker;
}

function start(options?: WorkerOptions): Worker {
  // Bundlers (Vite, webpack 5, Rollup) see this form and bundle the worker.
  return options?.worker ?? new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
}

export class WorkerTable<F extends Fields = Fields, C extends string = string> {
  readonly worker: Worker;
  #snapshot!: Snapshot;
  #position = new Map<string, number>();
  #watchers = new Set<(change: Change) => void>();
  #drawers = new Map<number, () => void>();
  #pending = new Map<number, Pending>();
  #nextCall = 1;
  #nextView = 1;
  #batch: { calls: Call[]; answered: Promise<void>; settle: () => void } | null = null;
  #ready: Promise<void>;

  /** A table of rows in a new worker. */
  static async create<const F extends Fields, const C extends string = never>(
    spec: TableSpec<F, C>,
    options?: WorkerOptions,
  ): Promise<WorkerTable<F, C>> {
    const table = new WorkerTable<F, C>(start(options), { op: "init", spec });
    await table.#ready;
    return table;
  }

  /** @internal The demo's trade book, made in a new worker (see `WorkerTradeBook`). */
  static async demo(options?: WorkerOptions): Promise<WorkerTable<any, any>> {
    const table = new WorkerTable(start(options), { op: "init", spec: null, demo: true });
    await table.#ready;
    return table;
  }

  private constructor(worker: Worker, init: ToWorker) {
    this.worker = worker;
    this.#ready = new Promise((resolve, reject) => {
      worker.onerror = (event) => reject(new Error(event.message || "the grid's worker failed to start"));
      worker.onmessage = (event: MessageEvent<FromWorker>) => {
        const message = event.data;
        if (message.type === "snapshot") {
          this.#take(message.snapshot);
          resolve();
        } else if (message.type === "reply") {
          const pending = this.#pending.get(message.id);
          this.#pending.delete(message.id);
          if (message.error !== undefined) pending?.reject(new Error(message.error));
          else pending?.resolve(message.results ?? []);
        }
      };
    });
    this.#send(init);
  }

  #send(message: ToWorker) {
    this.worker.postMessage(message);
  }

  #take(snapshot: Snapshot) {
    // The worker's clock starts elsewhere: on this page's, the recalculation
    // ended as the snapshot came, so "update and redraw" counts its time
    // in the worker and the drawing here (not the time in between).
    if (snapshot.change) snapshot.change.started = performance.now() - snapshot.change.ms;
    this.#snapshot = snapshot;
    this.#position = new Map((snapshot.columns as { name: string }[]).map((c, k) => [c.name, k]));
    if (snapshot.change) for (const watcher of [...this.#watchers]) watcher(snapshot.change);
    else for (const draw of this.#drawers.values()) draw();
  }

  /**
   * Sends calls to the worker; inside `batch`, gathers them instead. The
   * worker replies after sending the snapshot that includes the calls, so
   * when this resolves the page has it.
   */
  #calls(calls: Call[], batch = false, tolerant = false): Promise<unknown[]> {
    if (this.#batch) {
      this.#batch.calls.push(...calls);
      // Resolves when the batch is answered; if it fails, the batch's own promise says so.
      return this.#batch.answered.then(() => []);
    }
    const id = this.#nextCall++;
    const done = new Promise<unknown[]>((resolve, reject) => this.#pending.set(id, { resolve, reject }));
    this.#send({ op: "calls", id, calls, batch, tolerant });
    return done;
  }

  /** @internal Runs one call in the worker and gives back its result. */
  async call(on: Call["on"], method: string, ...args: unknown[]): Promise<any> {
    const [result] = await this.#calls([{ on, method, args }]);
    return result;
  }

  #col(name: string): number {
    const col = this.#position.get(name);
    if (col === undefined) throw new Error(`there is no column called ${name}`);
    return col;
  }

  // ------------------------------------------------------------------ writes

  /** Adds a row, or replaces the row with this key. */
  put(key: string, row: RowOf<F>): Promise<void> {
    return this.call("table", "put", key, row);
  }

  /** Adds or replaces many rows at once, from columns (see `Table.putMany`). */
  putMany(keys: readonly string[], columns: ColumnsOf<F>): Promise<void> {
    return this.call("table", "putMany", keys, columns);
  }

  /** Changes one field of a row. */
  set<N extends Extract<keyof F, string>>(key: string, field: N, value: ValueOf<F[N]>): Promise<void> {
    return this.call("table", "set", key, field, value);
  }

  /** Changes some fields of a row. */
  patch(key: string, values: Partial<RowOf<F>>): Promise<void> {
    return this.call("table", "patch", key, values);
  }

  /** Removes a row. Resolves to whether there was one. */
  drop(key: string): Promise<boolean> {
    return this.call("table", "drop", key);
  }

  setValue(name: string, value: number): Promise<void> {
    return this.call("table", "setValue", name, value);
  }

  addFormula(name: string, formula: string, total: Total | "none" = "none"): Promise<void> {
    return this.call("table", "addFormula", name, formula, total);
  }

  reserve(rows: number): Promise<void> {
    return this.call("table", "reserve", rows);
  }

  /**
   * Runs `edits` and sends them as one message: one recalculation, and one
   * step for undo. Inside it, writes are gathered rather than sent, and
   * their promises resolve once the batch has been answered.
   */
  batch(edits: () => void): Promise<void> {
    const outer = this.#batch === null;
    if (outer) {
      let settle!: () => void;
      const answered = new Promise<void>((resolve) => (settle = resolve));
      this.#batch = { calls: [], answered, settle };
    }
    const mine = this.#batch!;
    try {
      edits();
    } catch (error) {
      if (outer) {
        // Nothing was sent: nothing to wait for.
        this.#batch = null;
        mine.settle();
      }
      throw error;
    }
    if (!outer) return mine.answered;
    this.#batch = null;
    const sent = this.#calls(mine.calls, true);
    sent.then(mine.settle, mine.settle);
    return sent.then(() => undefined);
  }

  // ------------------------------------------------------------- the view

  /** Sorts the rows (see `Table.sort`), in the worker. */
  sort(by: { column: Name<F, C>; desc?: boolean }[]): Promise<void> {
    return this.call("table", "sort", by);
  }

  /** Filters a column (see `Table.filter`), in the worker. A bad filter rejects. */
  filter(column: Name<F, C>, text: string): Promise<void> {
    return this.call("table", "filter", column, text);
  }

  /** Shows only rows with this text (see `Table.search`), in the worker. */
  search(text: string): Promise<void> {
    return this.call("table", "search", text);
  }

  /** Groups the rows by a text column, or not at all (see `Table.group`), in the worker. */
  group(column: Name<F, C> | null): Promise<void> {
    return this.call("table", "group", column);
  }

  /** How the rows are shaped, as of the last snapshot. */
  viewState(): ViewState {
    return viewStateOf(this.#snapshot.viewState, this.columns().map((c) => c.name));
  }

  /** Asks the worker to recalculate now rather than at the end of its task. */
  flush(): void {
    this.#send({ op: "flush" });
  }

  // ------------------------------------------------------------------- reads

  /** How many rows there are, as of the last snapshot. */
  get size(): number {
    return this.#snapshot.rows;
  }

  columns(): Column[] {
    return (this.#snapshot.columns as { name: string; kind: Kind; formula: string | null; agg: Total | "none"; groupable: boolean }[]).map(
      (c) => ({ name: c.name, kind: c.kind, formula: c.formula, total: c.agg, groupable: c.groupable }),
    );
  }

  /** A column's total, as of the last snapshot. */
  total(column: Name<F, C>): number | null {
    return numberOf(this.totalExact(column));
  }

  /** A column's total as text, exactly (see `Table.totalExact`). */
  totalExact(column: Name<F, C>): string | null {
    return textOf(this.#snapshot.totals[this.#col(column)]);
  }


  /** How a column's total moved, as of the last snapshot (needs `history`). */
  totalTrend(column: Name<F, C>, points = 120): number[] {
    return (this.#snapshot.totalTrends[this.#col(column)] ?? []).slice(-points);
  }

  groups(): Group[] {
    const names = this.columns().map((c) => c.name);
    return (this.#snapshot.groupList as { g: string; n: number; c: (number | string | null)[] }[]).map((g) => ({
      key: g.g,
      rows: g.n,
      totals: Object.fromEntries(names.map((name, k) => [name, numberOf(textOf(g.c[k]))])),
    }));
  }

  history(): [oldest: number, latest: number] | null {
    return this.#snapshot.history;
  }

  /** Runs `read` against the last snapshot. Here every sync read already does. */
  peek<T>(read: (table: this) => T): T {
    return read(this);
  }

  /** A row's cells by column name, or `undefined` if there is no such row. */
  async get(key: string): Promise<(RowOf<F> & Record<C, Cell>) | undefined> {
    const json = (await this.call("engine", "row", key)) as string | undefined;
    if (json === undefined) return undefined;
    const cells = JSON.parse(json) as unknown[];
    return Object.fromEntries(this.columns().map((c, k) => [c.name, readCell(cells[k])])) as RowOf<F> & Record<C, Cell>;
  }

  why(key: string, column: Name<F, C>): Promise<string> {
    return this.call("engine", "explain", key, this.#col(column));
  }

  whyTotal(column: Name<F, C>): Promise<string> {
    return this.call("engine", "explain_total", this.#col(column));
  }

  csv(): Promise<string> {
    return this.call("engine", "csv");
  }

  async check(): Promise<string | null> {
    return ((await this.call("engine", "check")) as string | undefined) ?? null;
  }

  /** Calls `watcher` after every recalculation. Returns a function that stops it. */
  watch(watcher: (change: Change) => void): () => void {
    this.#watchers.add(watcher);
    return () => this.#watchers.delete(watcher);
  }

  /** Stops the worker. The table can't be used after. */
  dispose(): void {
    this.worker.terminate();
    for (const pending of this.#pending.values()) pending.reject(new Error("the table was disposed"));
    this.#pending.clear();
    this.#watchers.clear();
  }

  [Symbol.dispose](): void {
    this.dispose();
  }

  // -------------------------------------------------------------- for views

  /** @internal An engine for one view: what it draws comes from the snapshots. */
  connect(draw: () => void): RemoteEngine {
    const view = this.#nextView++;
    this.#drawers.set(view, draw);
    return new RemoteEngine(this, view, () => this.#snapshot, (message) => this.#send(message), () => this.#drawers.delete(view));
  }

  /** @internal Engine calls for a view, gathered into one message by `RemoteEngine.batch`. */
  engineCalls(calls: Call[], batch: boolean, tolerant = false): Promise<unknown[]> {
    return this.#calls(calls, batch, tolerant);
  }
}

/**
 * What a view sees of the engine of a WorkerTable. What it draws (the rows
 * in view, the counts, the totals) is read at once from the last snapshot;
 * when the rows in view are not in it, they are asked for, and the view is
 * drawn again when they come. Every other call goes to the worker and
 * returns a promise.
 */
export class RemoteEngine {
  #asked: { first: number; count: number } | null = null;
  #trendCols = new Set<number>();
  #points = 32;
  #gathered: Call[] | null = null;

  constructor(
    private readonly table: WorkerTable<any, any>,
    private readonly view: number,
    private readonly snapshot: () => Snapshot,
    private readonly send: (message: ToWorker) => void,
    private readonly close: () => void,
  ) {
    // Anything not defined here is the engine's own method, run in the worker.
    return new Proxy(this, {
      get: (target, name) => {
        if (name in target || typeof name !== "string") {
          // Bound to the object itself: its private fields are not the proxy's.
          const value = Reflect.get(target, name);
          return typeof value === "function" ? value.bind(target) : value;
        }
        return (...args: unknown[]) => target.#call(name, args);
      },
    });
  }

  #call(method: string, args: unknown[]): Promise<unknown> {
    const call: Call = { on: "engine", method, args };
    if (this.#gathered) {
      this.#gathered.push(call);
      return Promise.resolve(undefined);
    }
    return this.table.engineCalls([call], false).then(([result]) => result);
  }

  /** Runs `calls` (engine calls through this object) as one message: one step for undo. */
  batch(calls: () => void): Promise<void> {
    this.#gathered = [];
    let gathered: Call[];
    try {
      calls();
    } finally {
      gathered = this.#gathered;
      this.#gathered = null;
    }
    // In the worker, the table's batch makes them one step for undo.
    return gathered.length ? this.table.engineCalls(gathered, true).then(() => undefined) : Promise.resolve();
  }

  /**
   * Runs engine calls (`[method, ...args]` each) as one message, in order, and
   * gives back each one's result: nothing else runs in the worker between
   * them. With `batch` they are one step to undo; with `tolerant` a call that
   * fails gives `{ error }` and the rest still run.
   */
  sequence(calls: [string, ...unknown[]][], batch = false, tolerant = false): Promise<unknown[]> {
    return this.table.engineCalls(
      calls.map(([method, ...args]) => ({ on: "engine", method, args })),
      batch,
      tolerant,
    );
  }

  /** Lines of the view from the worker itself, not the snapshot: for ranges out of view. */
  async fetchRows(first: number, count: number): Promise<unknown[]> {
    return JSON.parse((await this.#call("rows", [first, count])) as string);
  }

  view_len(): number {
    return this.snapshot().viewLen;
  }
  row_count(): number {
    return this.snapshot().rows;
  }
  filtered_count(): number {
    return this.snapshot().filtered;
  }
  group_count(): number {
    return this.snapshot().groups;
  }
  columns(): unknown[] {
    return this.snapshot().columns;
  }
  totals(): (number | string | null)[] {
    return this.snapshot().totals;
  }
  history_range(): [number, number] | null {
    return this.snapshot().history;
  }
  total_trend(col: number, points: number): number[] | null {
    return this.snapshot().totalTrends[col]?.slice(-points) ?? null;
  }

  #window(): Window | undefined {
    return this.snapshot().windows[this.view];
  }

  /** Whether the window holds lines first..first+count (as far as the view goes). */
  #covers(w: Window | undefined, first: number, count: number): boolean {
    if (!w) return false;
    const end = Math.min(first + count, this.view_len());
    return w.first <= first && end <= w.first + w.lines.length;
  }

  #ask(first: number, count: number) {
    if (this.#asked && this.#asked.first <= first && first + count <= this.#asked.first + this.#asked.count) return;
    // Room either side, so scrolling a little needs no message.
    const from = Math.max(0, first - 2 * count);
    this.#asked = { first: from, count: 5 * count };
    this.send({
      op: "window",
      window: { view: this.view, first: from, count: 5 * count, trendCols: [...this.#trendCols], points: this.#points },
    });
  }

  /** The lines in view, from the last snapshot. Lines not in it yet come back blank, and are asked for. */
  rows(first: number, count: number): unknown[] {
    const w = this.#window();
    if (!this.#covers(w, first, count)) this.#ask(first, count);
    const total = Math.max(0, Math.min(count, this.view_len() - first));
    const blank = { k: "", c: (this.columns() as unknown[]).map(() => null) };
    return Array.from({ length: total }, (_, k) => {
      const at = first + k - (w?.first ?? 0);
      return w && at >= 0 && at < w.lines.length ? w.lines[at] : blank;
    });
  }

  trends(first: number, count: number, col: number, points: number): (number[] | null)[] {
    this.#points = points;
    const w = this.#window();
    if (!this.#trendCols.has(col) || (w && !(col in w.trends))) {
      this.#trendCols.add(col);
      this.#asked = null;
      this.#ask(first, count);
    }
    return Array.from({ length: count }, (_, k) => {
      const at = first + k - (w?.first ?? 0);
      return w?.trends[col]?.[at] ?? null;
    });
  }

  /** Forgets this view's rows. */
  disconnect() {
    this.send({ op: "unwindow", view: this.view });
    this.close();
  }
}
