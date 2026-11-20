import { createSignal, onCleanup, Show } from "solid-js";
import { WorkerTable } from "@surefold/grid";
import { Grid, createChange, createTotal } from "@surefold/grid-solid";

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

// The grid and figures, once the worker's table is there. A function called
// with the table, not a component given it: Solid warns when a prop is read as
// a component is made.
function book(trades: Trades) {
  const pnl = createTotal(trades, "pnl");
  const change = createChange(trades); // what the last recalculation did, in the worker

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
      <div class="bar">
        <button onClick={movePrices}>Move 1,000 prices</button>
        <span class="grow" />
        <span>
          P&amp;L <b>{pnl() === null ? "–" : whole.format(Math.round(pnl()!))}</b>
        </span>
        <Show when={change()}>
          {(c) => (
            <span class="muted">
              last update: {whole.format(c().recalculated)} nodes in {c().ms.toFixed(1)} ms
            </span>
          )}
        </Show>
      </div>
      <Grid class="grid" table={trades} keyColumn={{ title: "trade" }} columns={{ qty: { decimals: 0 }, pnl: { signed: true, width: 140 } }} />
    </>
  );
}

export function InAWorker() {
  const [trades, setTrades] = createSignal<Trades>();
  const [loading, setLoading] = createSignal("");
  let gone = false;
  onCleanup(() => {
    gone = true;
    trades()?.dispose(); // stops the worker
  });

  async function load() {
    setLoading("Starting the worker…");
    try {
      const table = await startTrades();
      if (gone) return table.dispose(); // the component went while the worker started
      setTrades(table);
      for (let first = 0; first < ROWS; first += BLOCK) {
        const { keys, columns } = block(first, BLOCK);
        // Sent to the worker, which adds the rows and recalculates there. Waiting for
        // each block keeps the page's memory small; the page is free meanwhile.
        await table.putMany(keys, columns);
        // Resolved once the page has the snapshot with these rows: size counts them.
        setLoading(`Loaded ${whole.format(table.size)} of ${whole.format(ROWS)} rows. Scroll, sort or click about meanwhile.`);
      }
      setLoading("");
    } catch (e) {
      // Disposing the table rejects what was waiting on its worker.
      if (!gone) setLoading(`Could not load: ${(e as Error).message}`);
    }
  }

  return (
    <Show
      when={trades()}
      keyed
      fallback={
        <div class="bar">
          <button disabled={loading() !== ""} onClick={load}>
            Load {whole.format(ROWS)} rows in a worker
          </button>
          <span class="muted">{loading()}</span>
        </div>
      }
    >
      {(t) => (
        <>
          <Show when={loading()}>
            <div class="bar muted">{loading()}</div>
          </Show>
          {book(t)}
        </>
      )}
    </Show>
  );
}
