import { createSignal, onCleanup } from "solid-js";
import { Duration, Effect, Fiber, Filter, Schedule, Schema, Stream } from "effect";
import { Socket } from "effect/socket";
import { Table } from "@surefold/grid";
import { Grid, createTotal } from "@surefold/grid-solid";

// The same feed, coins and table as the WebSocket recipe.
const FEED = "wss://ws-feed.exchange.coinbase.com";
const HOLDINGS: Record<string, number> = { "BTC-USD": 0.5, "ETH-USD": 8, "SOL-USD": 120, "DOGE-USD": 40000, "LTC-USD": 60, "LINK-USD": 400 };
const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

// A ticker message, as JSON text. Decoding checks its shape and reads the price as a
// finite number; the feed's other messages (the subscription's answer) do not decode.
const Ticker = Schema.Struct({ type: Schema.Literal("ticker"), product_id: Schema.String, price: Schema.FiniteFromString });
type Ticker = typeof Ticker.Type;
const asTicker = Filter.fromPredicateOption(Schema.decodeUnknownOption(Schema.fromJsonString(Ticker)));

/**
 * Coinbase's ticker as an Effect: it connects, reconnects with a backoff after
 * the connection drops, and hands `write` what came in each 50 ms. It knows
 * nothing of Solid or the table, and nothing happens until it is run.
 */
function livePrices(say: (status: string) => void, write: (batch: readonly Ticker[]) => void) {
  // One connection, as a stream of its messages. Effect's Socket dials when the
  // reader is acquired and closes the socket when the stream ends, fails or is
  // interrupted. Every close fails the stream (the server's included), so that
  // `retry` below dials again. What runs between dialling and the first read
  // runs once a connection: here, the subscription.
  const connection = Stream.unwrap(
    Effect.gen(function* () {
      say("Connecting…");
      const socket = yield* Socket.makeWebSocket(FEED);
      const read = yield* Socket.readerString(socket); // dials, and waits for the socket to open
      const { write: send } = yield* socket.writer;
      yield* send(JSON.stringify({ type: "subscribe", product_ids: Object.keys(HOLDINGS), channels: ["ticker"] }));
      say("Live");
      // Each read is the frames that came in together; one message a frame.
      return Stream.fromEffectRepeat(read).pipe(Stream.flattenIterable);
    }),
  );

  // Wait 0.5 s before trying again, then 1, 2, 4… up to 10 s, each give or take 20%,
  // so that many pages that lost the server at once do not all come back together.
  const backoff = Schedule.min([Schedule.exponential("500 millis"), Schedule.spaced("10 seconds")]).pipe(
    Schedule.jittered,
    Schedule.tap(({ duration, attempt }) =>
      Effect.sync(() => say(`Reconnecting in ${(Duration.toMillis(duration) / 1000).toFixed(1)} s (attempt ${attempt})`)),
    ),
  );

  return connection.pipe(
    Stream.retry(backoff), // the backoff starts again from 0.5 s once a message comes through
    Stream.filterMap(asTicker), // tickers only, decoded
    Stream.groupedWithin(500, "50 millis"), // what came in 50 ms (or 500 messages), as one array
    Stream.runForEach((batch) => Effect.sync(() => write(batch))),
    Effect.provide(Socket.layerWebSocketConstructorGlobal), // the browser's WebSocket
  );
}

export function EffectPrices() {
  const coins = new Table({
    fields: { units: "number", open: "number", price: "number" },
    formulas: {
      value: "units * price",
      gain: "units * (price - open)", // since this page connected
    },
    totals: { value: "sum", gain: "sum" },
    history: 120, // for each price's trend in its cell
  });
  const gain = createTotal(coins, "gain");
  const [status, setStatus] = createSignal("Not connected");
  const [ticks, setTicks] = createSignal(0);
  const [connected, setConnected] = createSignal(false);
  let running: Fiber.Fiber<void, Error> | undefined;

  // Keep the latest price of each coin in a batch, and write them all at once (one recalculation).
  function write(batch: readonly Ticker[]) {
    const latest = new Map(batch.map((ticker) => [ticker.product_id, ticker.price]));
    coins.batch(() => {
      for (const [coin, price] of latest) {
        if (coins.get(coin)) coins.set(coin, "price", price);
        else coins.put(coin, { units: HOLDINGS[coin], open: price, price }); // its first price: add its row
      }
    });
    setTicks((n) => n + batch.length);
  }

  // Connect runs it as a fiber. Interrupting the fiber closes the socket, or stops the wait to reconnect.
  function connect() {
    setConnected(true);
    running = Effect.runFork(livePrices(setStatus, write));
  }
  function disconnect() {
    if (running) Effect.runFork(Fiber.interrupt(running));
    running = undefined;
    setConnected(false);
    setStatus("Not connected");
  }

  onCleanup(() => {
    disconnect();
    coins.dispose();
  });

  return (
    <>
      <div class="bar">
        <button onClick={() => (connected() ? disconnect() : connect())}>{connected() ? "Disconnect" : "Connect"}</button>
        <span class="muted">
          {status()} · {ticks()} messages
        </span>
        <span class="grow" />
        <span>
          Gain since connecting: <b>{gain() === null ? "–" : money.format(gain()!)}</b>
        </span>
      </div>
      <Grid
        class="grid"
        table={coins}
        keyColumn={{ title: "coin", width: 110 }}
        columns={{ units: { decimals: 2 }, open: { decimals: 4 }, price: { decimals: 4, trend: true, width: 150 }, gain: { signed: true } }}
      />
    </>
  );
}
