// The watershed grid in Solid. The table holds the rows; <Grid> draws it.
//
//   const trades = new Table({ fields: { qty: "number", price: "number" }, totals: { qty: "sum" } });
//   const qty = createTotal(trades, "qty");      // a signal that follows the total
//   const groups = createGroups(trades);         // changes only when a group does
//   <Grid table={trades} theme="dark" columns={{ qty: { decimals: 0 } }} />

import { createEffect, createSignal, onCleanup, untrack, type Accessor } from "solid-js";
import { spread, type JSX } from "@solidjs/web";
import { drawGrid, type AnyGridTable, type Change, type GridOptions, type GridView, type Group } from "@surefold/grid";

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
  spread(root, { get style() { return props.style; } }, true);
  // The view puts classes of its own on the element, so `class` adds and takes
  // away only its own names rather than replacing them all.
  createEffect(
    () => (props.class ?? "").split(/\s+/).filter(Boolean),
    (names, before = []) => {
      root.classList.remove(...before.filter((name) => !names.includes(name)));
      root.classList.add(...names);
    },
  );
  let view: GridView | undefined;
  createEffect(
    // Only a new table draws again: the rest is read once, when it does.
    () => [props.table, untrack(() => ({ keyColumn: props.keyColumn, columns: props.columns, theme: props.theme, rowHeight: props.rowHeight, label: props.label, paged: Boolean(props.loadMore) }))] as const,
    ([table, { paged, ...options }]) => {
      const drawn = (view = drawGrid(root, table, {
        ...options,
        // Read the props when called, so a new handler is used without drawing again.
        loadMore: paged ? () => props.loadMore!() : undefined,
        onStatus: (s) => props.onStatus?.(s),
        onFocus: (f) => props.onFocus?.(f),
        onGroup: (c) => props.onGroup?.(c),
        onError: (m) => (props.onError ? props.onError(m) : console.warn(m)),
        onNote: (m) => props.onNote?.(m),
        onEdit: (e) => props.onEdit?.(e),
        rowClass: (key) => props.rowClass?.(key),
      }));
      props.ref?.(drawn);
      return () => drawn.destroy();
    },
  );
  createEffect(() => props.theme, (theme) => view?.setTheme(theme ?? ""), { defer: true });
  return root;
}

/** The last recalculation of a table, as a signal; `undefined` until there is one. */
export function createChange(table: AnyGridTable): Accessor<Change | undefined> {
  // ownedWrite: the table may be changed, and so this set, while a component is being made.
  const [change, setChange] = createSignal<Change | undefined>(undefined, { ownedWrite: true });
  onCleanup(table.watch(setChange));
  return change;
}

/**
 * Follows anything read from a table, such as `(t) => t.total("pnl")`, as a
 * signal. It is set after every recalculation; with `isEqual`, only when the
 * value compares unequal to the last, so what follows it runs only then.
 * The reading must not itself be a function: Solid takes a function as a
 * signal's value to be a calculation.
 */
export function createReading<T, Tb extends AnyGridTable = AnyGridTable>(
  table: Tb,
  read: (table: Tb) => T,
  isEqual?: (a: T, b: T) => boolean,
): Accessor<T> {
  const [value, setValue] = createSignal<T>(read(table) as Exclude<T, Function>, { equals: isEqual ?? false, ownedWrite: true });
  onCleanup(table.watch(() => setValue(() => read(table))));
  return value;
}

/**
 * The table's groups (`groups()`), each with its row count and totals, as a
 * signal that changes only when a group, a count or a total changed.
 */
export function createGroups(table: AnyGridTable): Accessor<Group[]> {
  return createReading(table, (t) => t.groups(), sameGroups);
}

/** Whether two lists of groups have the same keys, counts and totals, in the same order. */
function sameGroups(a: Group[], b: Group[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((g, k) => {
    const h = b[k];
    if (g.key !== h.key || g.rows !== h.rows) return false;
    const names = Object.keys(g.totals);
    return names.length === Object.keys(h.totals).length && names.every((name) => g.totals[name] === h.totals[name]);
  });
}

/** A column's total, as a signal. */
export function createTotal(table: AnyGridTable, column: string): Accessor<number | null> {
  return createReading(table, (t) => t.total(column));
}
