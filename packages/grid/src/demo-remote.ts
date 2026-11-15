// The demo's trade book with its rows in a Web Worker: the same book as
// `TradeBook`, made, filled and moved in the worker, so building a hundred
// thousand rows or streaming thousands of prices a second leaves the page free.

import { WorkerTable, type WorkerOptions } from "./remote.js";
import type { TradeFields } from "./demo.js";

export class WorkerTradeBook {
  private constructor(readonly table: WorkerTable<TradeFields, "notional" | "pnl" | "net">) {}

  static async create(options?: WorkerOptions): Promise<WorkerTradeBook> {
    return new WorkerTradeBook(await WorkerTable.demo(options));
  }

  /** As `TradeBook.streamingRows`. */
  streamingRows(): Promise<void> {
    return this.table.call("demo", "streamingRows");
  }

  /** Adds `count` more trades, in the worker. */
  fill(count: number): Promise<void> {
    return this.table.call("demo", "fill", count);
  }

  /** Moves the mark of `changes` trades picked at random, in the worker. */
  tick(changes: number): Promise<void> {
    return this.table.call("demo", "tick", changes);
  }
}
