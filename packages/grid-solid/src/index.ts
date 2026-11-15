// The watershed grid in Solid. The table holds the rows; <Grid> draws it.
//
//   const trades = new Table({ fields: { qty: "number", price: "number" }, totals: { qty: "sum" } });
//   const qty = createTotal(trades, "qty");      // a signal that follows the total
//   <Grid table={trades} theme="dark" columns={{ qty: { decimals: 0 } }} />

import { createEffect, createSignal, on, onCleanup, type Accessor, type JSX } from "solid-js";
import { spread } from "solid-js/web";
import { drawGrid, type AnyGridTable, type Change, type GridOptions, type GridView } from "@surefold/grid";

export interface GridProps extends Omit<GridOptions, "theme"> {
  table: AnyGridTable;
  theme?: string;
  /** Called with the view once it is drawn, for its methods (grouping, looking back…). */
  ref?: (view: GridView) => void;
  class?: string;
  style?: JSX.CSSProperties;
}

/**
 * Draws a table. A new `table` draws a new grid; a new `theme` restyles it.
 * `columns`, `key` and the rest are read when the grid is drawn.
 */
export function Grid(props: GridProps): JSX.Element {
  // Written without JSX, so the package is plain JavaScript that any bundler takes.
  const root = document.createElement("div");
  spread(root, { get style() { return props.style; } }, false, false);
  // The view puts classes of its own on the element, so `class` adds and takes
  // away only its own names rather than replacing them all.
  createEffect<string[]>((before) => {
    const names = (props.class ?? "").split(/\s+/).filter(Boolean);
    root.classList.remove(...before.filter((name) => !names.includes(name)));
    root.classList.add(...names);
    return names;
  }, []);
  createEffect(
    on(
      () => props.table,
      (table) => {
        const view = drawGrid(root, table, {
          keyColumn: props.keyColumn,
          columns: props.columns,
          theme: props.theme,
          rowHeight: props.rowHeight,
          label: props.label,
          // Read the props when called, so a new handler is used without drawing again.
          loadMore: props.loadMore && (() => props.loadMore!()),
          onStatus: (s) => props.onStatus?.(s),
          onFocus: (f) => props.onFocus?.(f),
          onGroup: (c) => props.onGroup?.(c),
          onError: (m) => (props.onError ? props.onError(m) : console.warn(m)),
          onNote: (m) => props.onNote?.(m),
        });
        createEffect(on(() => props.theme, (theme) => view.setTheme(theme ?? ""), { defer: true }));
        props.ref?.(view);
        onCleanup(() => view.destroy());
      },
    ),
  );
  return root;
}

/** The last recalculation of a table, as a signal; `undefined` until there is one. */
export function createChange(table: AnyGridTable): Accessor<Change | undefined> {
  const [change, setChange] = createSignal<Change>();
  onCleanup(table.watch(setChange));
  return change;
}

/** Follows anything read from a table, such as `(t) => t.total("pnl")`, as a signal. */
export function createReading<T>(table: AnyGridTable, read: (table: AnyGridTable) => T): Accessor<T> {
  const [value, setValue] = createSignal<T>(read(table), { equals: false });
  onCleanup(table.watch(() => setValue(() => read(table))));
  return value;
}

/** A column's total, as a signal. */
export function createTotal(table: AnyGridTable, column: string): Accessor<number | null> {
  return createReading(table, (t) => t.total(column));
}
