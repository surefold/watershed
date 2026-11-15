import type { AnyGridTable, Change } from "./table.js";

export interface Theme {
  /** What `setTheme` takes; "" takes the page's colours. */
  id: string;
  name: string;
}

/** The themes grid.css has, for a list to choose from. */
export declare const themes: Theme[];

/** How one column looks, by its name. */
export interface ColumnLook {
  /** Places after the point. 2 if not given. */
  decimals?: number;
  /** Colours a number by its sign. */
  signed?: boolean;
  /** Width in pixels. */
  width?: number;
  /** Draws how the cell moved over recent recalculations (needs the table's `history`). */
  trend?: boolean;
}

/** What a view says as it goes; each call carries only some of these. */
export interface Status extends Partial<Change> {
  /** The time to recalculate and redraw, in milliseconds. */
  ms?: number;
  rows?: number;
  /** Rows that pass the filters. */
  filtered?: number;
  groups?: number;
  /** Rows selected (click a key). */
  selected?: number;
  /** A block of cells picked out by dragging: their count and, if numbers, sum, avg, min and max. */
  range?: { cells: number; numbers: number; sum: number; avg: number; min: number; max: number } | null;
  /** While rows load as the person scrolls. */
  loading?: boolean;
  done?: boolean;
}

/** The cell, group total or bottom-row total the person is on. */
export interface Focus {
  line: number;
  pos: number;
  kind: "cell" | "group" | "total";
  key: string | null;
  col: number;
}

/** How the columns are laid out, by column name; "" is the key column in `widths`. */
export interface Layout {
  order: string[];
  hidden: string[];
  pinned: string[];
  widths: Record<string, number>;
}

export interface GridOptions {
  /** The first column, which shows each row's key. */
  keyColumn?: { title?: string; width?: number };
  /** How each column looks, by name. */
  columns?: Record<string, ColumnLook>;
  /** One of `themes`' ids, or a theme of your own (a `[data-wg-theme="name"]` rule). */
  theme?: string;
  rowHeight?: number;
  /** What a screen reader calls the grid. */
  label?: string;
  /** Called as the person nears the end: add rows to the table, and resolve to whether there may be more. */
  loadMore?: () => Promise<boolean>;
  onStatus?: (status: Status) => void;
  onFocus?: (focus: Focus | null) => void;
  /** Rows were grouped by a column (its position), or -1 for none. */
  onGroup?: (col: number) => void;
  /** Something went wrong, said for a person. */
  onError?: (message: string) => void;
  /** Something worked (copied, pasted, undone), said for a person. */
  onNote?: (message: string) => void;
}

/** Draws `table` into `root`, which must have a height. */
export declare function drawGrid(root: HTMLElement, table: AnyGridTable, options?: GridOptions): GridView;

export declare class GridView {
  constructor(root: HTMLElement, table: AnyGridTable, options?: GridOptions);
  setTheme(theme: string): void;
  /** Recalculates now and redraws. Not needed after changes made through the table. In a worker, the redraw comes when the worker answers. */
  refresh(): Change | void;
  getLayout(): Layout;
  setLayout(layout: Partial<Layout>): void;
  setPinned(col: number, pinned: boolean): void;
  setHidden(col: number, hidden: boolean): void;
  /**
   * Fits a column's width to its header, its total and the rows drawn now
   * (`col` is its position; -1 for the key column). People can double-click
   * a column's edge for the same.
   */
  fitColumn(col: number): void;
  /** Fits every column on screen. */
  fitColumns(): void;
  /** Sorts by these columns (positions), the first mattering most. */
  setSort(sort: { col: number; desc: boolean }[]): void;
  /** Filters a column: `>100`, `5..10`, `=Rates`, or text to look for. */
  setFilter(col: number, text: string): void;
  /** Keeps the rows whose key or text contains this. */
  setQuickFilter(text: string): void;
  /** Groups by the column at this position, or by nothing if negative. */
  setGroup(col: number): void;
  expandAll(open: boolean): void;
  /** Opens a group and scrolls to it. */
  revealGroup(key: string): Promise<void>;
  /** Shows the table as it was after an earlier recalculation (see `table.history()`), or now for `null`. */
  setAsOf(epoch: number | null): void;
  /**
   * Why the focused cell or total has its value. With a WorkerTable the
   * answer comes from the worker: until it does, this says so, and `onFocus`
   * is called again when it comes.
   */
  explainFocus(): { title: string; text: string } | null;
  selectedKeys(): string[];
  removeSelected(): void;
  /** Asks `loadMore` for more rows now. */
  fetchMore(): Promise<void>;
  destroy(): void;
}
