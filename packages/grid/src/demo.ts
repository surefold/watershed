// The trade book the site's grid opens with. Its rows are made, and its
// marks moved, inside WebAssembly, so a hundred thousand rows build quickly
// and thousands of prices a second can stream.

import { Grid } from "../pkg/watershed_grid.js";
import { Table } from "./table.js";

export type TradeFields = {
  symbol: "text";
  desk: "text";
  trader: "text";
  side: "text";
  qty: "number";
  price: "number";
  mark: "number";
};

export class TradeBook {
  readonly table: Table<TradeFields, "notional" | "pnl" | "net">;

  constructor() {
    this.table = new Table(null, Grid.demo()) as Table<TradeFields, "notional" | "pnl" | "net">;
  }

  /**
   * For rows that keep arriving: don't keep a node per row per column for
   * every group's totals, which would stay for good. Groups add up the plain way.
   */
  streamingRows(): void {
    this.table.engine.set_group_budget(0);
  }

  /** Adds `count` more trades, `T-000001` onwards. */
  fill(count: number): void {
    this.table.engine.demo_fill(count);
    this.table.touched();
  }

  /** Moves the mark of `changes` trades picked at random, each by up to 0.3%. */
  tick(changes: number): void {
    this.table.engine.demo_tick(changes);
    this.table.touched();
  }
}
