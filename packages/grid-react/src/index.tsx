// The watershed grid in React. The table holds the rows; <Grid> draws it.
// Rows never go through props or state: change them on the table, and the
// grid and any `useTotal` follow.
//
//   const trades = useTable(() => new Table({ fields: { qty: "number" }, totals: { qty: "sum" } }));
//   const qty = useTotal(trades, "qty");
//   const groups = useGroups(trades);       // a new array only when a group changes
//   <Grid table={trades} theme="dark" />

import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties } from "react";
import { drawGrid, type AnyGridTable, type AnyTable, type Change, type GridOptions, type GridView, type Group } from "@surefold/grid";

export interface GridProps extends Omit<GridOptions, "theme"> {
  table: AnyGridTable;
  theme?: string;
  className?: string;
  style?: CSSProperties;
}

/**
 * Draws a table. A new `table` draws a new grid; a new `theme` restyles it.
 * `columns`, `key` and the rest are read when the grid is drawn. The ref is
 * the view, for its methods (grouping, looking back…).
 */
export const Grid = forwardRef<GridView | null, GridProps>(function Grid(props, ref) {
  const root = useRef<HTMLDivElement>(null);
  const latest = useRef(props);
  latest.current = props;
  const [view, setView] = useState<GridView | null>(null);

  useLayoutEffect(() => {
    const p = latest.current;
    const v = drawGrid(root.current!, props.table, {
      keyColumn: p.keyColumn,
      columns: p.columns,
      theme: p.theme,
      rowHeight: p.rowHeight,
      label: p.label,
      // Read the props when called, so a new handler is used without drawing again.
      loadMore: p.loadMore && (() => latest.current.loadMore!()),
      onStatus: (s) => latest.current.onStatus?.(s),
      onFocus: (f) => latest.current.onFocus?.(f),
      onGroup: (c) => latest.current.onGroup?.(c),
      onError: (m) => (latest.current.onError ? latest.current.onError(m) : console.warn(m)),
      onNote: (m) => latest.current.onNote?.(m),
      onEdit: (e) => latest.current.onEdit?.(e),
    });
    setView(v);
    return () => {
      v.destroy();
      setView(null);
    };
  }, [props.table]);

  useEffect(() => view?.setTheme(props.theme ?? ""), [view, props.theme]);
  useImperativeHandle(ref, () => view!, [view]);

  // The view puts classes of its own on the element it draws into, so that is
  // an inner one: React's className and style go on the outer one.
  return (
    <div className={props.className} style={props.style}>
      <div ref={root} style={{ height: "100%" }} />
    </div>
  );
});

/**
 * A table made once for this component, freed when it goes. (Made outside a
 * component, a table lives as long as the page, and needs neither.)
 */
export function useTable<T extends AnyTable>(make: () => T): T {
  const [table] = useState(make);
  const pending = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => {
    // Strict mode unmounts and mounts again at once: only free it if it stays gone.
    clearTimeout(pending.current);
    return () => {
      pending.current = setTimeout(() => table.dispose());
    };
  }, [table]);
  return table;
}

/**
 * Follows anything read from a table, such as `(t) => t.total("pnl")`. It
 * reads the table as of its last recalculation (`peek`), so rendering never
 * sets off a recalculation, and renders again after the next one if the value
 * changed. Values are compared with `===`; for a list or an object, pass
 * `isEqual`, and the value stays the same object until it compares unequal.
 */
export function useReading<T, Tb extends AnyGridTable = AnyGridTable>(
  table: Tb,
  read: (table: Tb) => T,
  isEqual?: (a: T, b: T) => boolean,
): T {
  const reader = useRef(read);
  reader.current = read;
  const same = useRef(isEqual);
  same.current = isEqual;
  const last = useRef<{ table: Tb; value: T }>(undefined);
  const subscribe = useCallback((changed: () => void) => table.watch(changed), [table]);
  const snapshot = () => {
    const value = (table as AnyGridTable).peek(() => reader.current(table));
    const before = last.current;
    // The same value as last time, if it compares equal, so React sees no change.
    if (before && before.table === table && same.current?.(before.value, value)) return before.value;
    last.current = { table, value };
    return value;
  };
  return useSyncExternalStore(subscribe, snapshot);
}

/** A column's total. */
export function useTotal(table: AnyGridTable, column: string): number | null {
  return useReading(table, (t) => t.total(column));
}

/**
 * The table's groups (`groups()`), each with its row count and totals. The
 * list is a new array only when a group, a count or a total changed, so it
 * can go to a chart, `useMemo` or `memo` as it is.
 */
export function useGroups(table: AnyGridTable): Group[] {
  return useReading(table, (t) => t.groups(), sameGroups);
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

/** The last recalculation of a table; `undefined` until there is one. */
export function useChange(table: AnyGridTable): Change | undefined {
  const [change, setChange] = useState<Change>();
  useEffect(() => table.watch(setChange), [table]);
  return change;
}
