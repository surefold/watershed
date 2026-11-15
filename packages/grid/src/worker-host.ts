// What the worker that holds a WorkerTable's rows does (see worker.ts).
// The engine runs here, so a large load or a long recalculation never stops
// the page; after each recalculation the page is sent what its views draw.

import { Table } from "./table.js";
import { TradeBook } from "./demo.js";
import type { Call, FromWorker, Snapshot, ToWorker, Window, WindowRequest } from "./protocol.js";
import type { Change } from "./table.js";

/** How many recalculations back a column total's trend goes, for charts. */
const TOTAL_TREND = 120;

/** Engine calls that change what the views show, and so recalculate after. */
const CHANGES = new Set([
  "set_cell", "set_sort", "set_filter", "set_quick_filter", "set_group", "set_group_budget",
  "toggle_group", "expand_all", "set_as_of", "set_agg", "remove", "fill", "undo", "redo",
  "begin_edits", "end_edits", "keep_history",
]);

let table: Table<any, any> | undefined;
let book: TradeBook | undefined;
const windows = new Map<number, WindowRequest>();

function post(message: FromWorker) {
  (self as unknown as Worker).postMessage(message);
}

function window(w: WindowRequest): Window {
  const engine = table!.engine;
  const lines = JSON.parse(engine.rows(w.first, w.count)) as unknown[];
  const trends: Record<number, (number[] | null)[]> = {};
  for (const col of w.trendCols) trends[col] = JSON.parse(engine.trends(w.first, lines.length, col, w.points));
  return { first: w.first, lines, trends };
}

function snapshot(change: Change | null): Snapshot {
  const t = table!;
  const engine = t.engine;
  const columns = JSON.parse(engine.columns()) as { agg: string }[];
  const history = t.history();
  const totalTrends: Record<number, number[] | null> = {};
  if (history) {
    columns.forEach((c, col) => {
      if (c.agg !== "none") totalTrends[col] = JSON.parse(engine.total_trend(col, TOTAL_TREND));
    });
  }
  const groups = engine.group_count();
  return {
    change,
    rows: engine.row_count(),
    filtered: engine.filtered_count(),
    groups,
    viewLen: engine.view_len(),
    columns,
    totals: JSON.parse(engine.totals()),
    totalTrends,
    history,
    groupList: groups > 0 ? JSON.parse(engine.groups()) : [],
    viewState: engine.view_state(),
    windows: Object.fromEntries([...windows].map(([view, w]) => [view, window(w)])),
  };
}

function send(change: Change | null) {
  post({ type: "snapshot", snapshot: snapshot(change) });
}

function run(call: Call): unknown {
  const target: any = call.on === "table" ? table : call.on === "demo" ? book : table!.engine;
  const method = target?.[call.method];
  if (typeof method !== "function") throw new Error(`no ${call.on} method ${call.method}`);
  const result = method.apply(target, call.args);
  if (call.on === "engine" && CHANGES.has(call.method)) {
    if (call.method === "set_agg") table!.columnsChanged();
    else table!.touched();
  }
  return result;
}

/** Handles one message from the page. */
export function handle(message: ToWorker) {
  switch (message.op) {
    case "init":
      if (message.demo) {
        book = new TradeBook();
        table = book.table;
      } else {
        table = new Table(message.spec!);
      }
      table.watch((change) => send(change));
      post({ type: "ready" });
      send(table.flush());
      break;
    case "calls": {
      const results: unknown[] = [];
      try {
        const one = (call: Call) => {
          if (!message.tolerant) return run(call);
          try {
            return run(call);
          } catch (error) {
            return { error: (error as Error).message };
          }
        };
        const all = () => message.calls.forEach((call) => results.push(one(call)));
        if (message.batch) table!.batch(all);
        else all();
        post({ type: "reply", id: message.id, results });
      } catch (error) {
        post({ type: "reply", id: message.id, error: (error as Error).message });
      }
      break;
    }
    case "window":
      windows.set(message.window.view, message.window);
      // The rows asked for, as they are now: nothing was recalculated.
      send(null);
      break;
    case "unwindow":
      windows.delete(message.view);
      break;
    case "flush":
      table!.flush();
      break;
  }
}
