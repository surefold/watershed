import { useEffect, useRef, useState } from "react";
import { WorkerTable } from "@surefold/grid";
import { Grid, useChange, useTotal } from "@surefold/grid-react";

const ROWS = 300_000;
const BLOCK = 50_000;
const SYMBOLS = ["OAKS", "ELM", "BIRCH", "ASH", "PINE", "YEW", "FIR", "LIME"];
const whole = new Intl.NumberFormat("en-US");

// A WorkerTable takes the same spec as a Table; its engine starts in a new Web Worker.
const startTrades = () =>
  WorkerTable.create({
    fields: { symbol: "text", qty: "number", price: "number", mark: "number" },
    formulas: { pnl: "qty * (mark - price)" },
    totals: { qty: "sum", pnl: "sum" },
  });
type Trades = Awaited<ReturnType<typeof startTrades>>;

/** One block of made-up trades, a column at a time. Yours would come from a fetch. */
function block(first: number, count: number) {
  const keys: string[] = [];
  const symbol: string[] = [];
  const [qty, price, mark] = [new Float64Array(count), new Float64Array(count), new Float64Array(count)];
  for (let k = 0; k < count; k++) {
    keys.push(`T-${first + k}`);
    symbol.push(SYMBOLS[(first + k) % SYMBOLS.length]);
    qty[k] = 100 * (1 + ((first + k) % 20));
    price[k] = 90 + ((first + k) % 200) / 10;
    mark[k] = price[k] + (Math.random() - 0.5) * 4;
  }
  return { keys, columns: { symbol, qty, price, mark } };
}

/** The grid and figures, once the worker's table is there. */
function Book({ trades }: { trades: Trades }) {
  const pnl = useTotal(trades, "pnl");
  const change = useChange(trades); // what the last recalculation did, in the worker

  function movePrices() {
    // One message to the worker: one recalculation, one step for undo.
    void trades.batch(() => {
      for (let k = 0; k < 1000; k++) {
        const row = Math.floor(Math.random() * trades.size);
        trades.set(`T-${row}`, "mark", 90 + Math.random() * 25);
      }
    });
  }

  return (
    <>
      <div className="bar">
        <button onClick={movePrices}>Move 1,000 prices</button>
        <span className="grow" />
        <span>
          P&amp;L <b>{pnl === null ? "–" : whole.format(Math.round(pnl))}</b>
        </span>
        {change && (
          <span className="muted">
            last update: {whole.format(change.recalculated)} nodes in {change.ms.toFixed(1)} ms
          </span>
        )}
      </div>
      <Grid className="grid" table={trades} keyColumn={{ title: "trade" }} columns={{ qty: { decimals: 0 }, pnl: { signed: true, width: 140 } }} />
    </>
  );
}

export function InAWorker() {
  const [trades, setTrades] = useState<Trades>();
  const [loading, setLoading] = useState("");
  const gone = useRef(false);
  const table = useRef<Trades>(undefined);

  // Stop the worker when the component goes.
  useEffect(() => {
    gone.current = false;
    return () => {
      gone.current = true;
      table.current?.dispose();
      table.current = undefined;
    };
  }, []);

  async function load() {
    setLoading("Starting the worker…");
    try {
      const t = await startTrades();
      if (gone.current) return t.dispose(); // the component went while the worker started
      table.current = t;
      setTrades(t);
      for (let first = 0; first < ROWS; first += BLOCK) {
        const { keys, columns } = block(first, BLOCK);
        // Sent to the worker, which adds the rows and recalculates there. Waiting for
        // each block keeps the page's memory small; the page is free meanwhile.
        await t.putMany(keys, columns);
        setLoading(`Loaded ${whole.format(first + BLOCK)} of ${whole.format(ROWS)} rows. Scroll, sort or click about meanwhile.`);
      }
      setLoading("");
    } catch (e) {
      // Disposing the table rejects what was waiting on its worker.
      if (!gone.current) setLoading(`Could not load: ${(e as Error).message}`);
    }
  }

  if (!trades) {
    return (
      <div className="bar">
        <button disabled={loading !== ""} onClick={load}>
          Load {whole.format(ROWS)} rows in a worker
        </button>
        <span className="muted">{loading}</span>
      </div>
    );
  }
  return (
    <>
      {loading && <div className="bar muted">{loading}</div>}
      <Book trades={trades} />
    </>
  );
}
