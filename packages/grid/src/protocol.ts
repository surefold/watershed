// The messages between a WorkerTable (on the page) and its worker.
// Everything the view draws comes over in a snapshot after each
// recalculation; everything else is a call with a reply.

import type { Change, TableSpec } from "./table.js";

/** One call on the worker's table (`table.put(...)`) or its engine (`engine.set_sort(...)`). */
export interface Call {
  on: "table" | "engine" | "demo";
  method: string;
  args: unknown[];
}

/** The rows a view shows, and which columns draw trends. */
export interface WindowRequest {
  view: number;
  first: number;
  count: number;
  trendCols: number[];
  points: number;
}

export type ToWorker =
  | { op: "init"; spec: TableSpec<any, any> | null; demo?: boolean }
  /**
   * Calls run in order; with `batch`, as one step for undo and one
   * recalculation. With `tolerant`, a call that fails gives `{ error }` as its
   * result and the rest still run.
   */
  | { op: "calls"; id: number; calls: Call[]; batch?: boolean; tolerant?: boolean }
  | { op: "window"; window: WindowRequest }
  | { op: "unwindow"; view: number }
  | { op: "flush" };

/** A view's rows as the worker last saw them. */
export interface Window {
  first: number;
  /** As the engine gives them: `{k, c}` for a row, `{g, n, o, c}` for a group. */
  lines: unknown[];
  /** For each trend column, each line's trend. */
  trends: Record<number, (number[] | null)[]>;
}

/** What the page knows of the table between recalculations. */
export interface Snapshot {
  change: Change | null;
  rows: number;
  filtered: number;
  groups: number;
  viewLen: number;
  columns: unknown[];
  /** Each column's total: a number, a decimal as text, or null. */
  totals: (number | string | null)[];
  /** Each column's total over the last 120 recalculations (with history). */
  totalTrends: Record<number, number[] | null>;
  history: [number, number] | null;
  /** The groups and their totals, when rows are grouped. */
  groupList: unknown[];
  /** The sort, filters, search and grouping, as the engine gives them. */
  viewState: string;
  windows: Record<number, Window>;
}

export type FromWorker =
  | { type: "ready" }
  | { type: "snapshot"; snapshot: Snapshot }
  | { type: "reply"; id: number; results?: unknown[]; error?: string };
