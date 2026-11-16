import { createSignal, onCleanup } from "solid-js";
import { Table } from "@surefold/grid";
import { Grid, createTotal } from "@surefold/grid-solid";

// Coinbase's public ticker: a message each time a coin trades. No key needed.
const FEED = "wss://ws-feed.exchange.coinbase.com";
const HOLDINGS: Record<string, number> = { "BTC-USD": 0.5, "ETH-USD": 8, "SOL-USD": 120, "DOGE-USD": 40000, "LTC-USD": 60, "LINK-USD": 400 };
const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

export function WebSocketPrices() {
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
  const [state, setState] = createSignal("Not connected");
  const [ticks, setTicks] = createSignal(0);
  const [connected, setConnected] = createSignal(false);

  let socket: WebSocket | undefined;
  let frame = 0;
  const latest = new Map<string, number>(); // coin → its latest price, since the last frame

  // Many messages can come in a frame: keep the latest price of each coin, and
  // write them all to the table once a frame, as one batch (one recalculation).
  function flush() {
    frame = 0;
    coins.batch(() => {
      for (const [coin, price] of latest) {
        if (coins.get(coin)) coins.set(coin, "price", price);
        else coins.put(coin, { units: HOLDINGS[coin], open: price, price }); // its first price: add its row
      }
    });
    latest.clear();
  }

  function connect() {
    setState("Connecting…");
    setConnected(true);
    socket = new WebSocket(FEED);
    socket.onopen = () => {
      setState("Live");
      socket!.send(JSON.stringify({ type: "subscribe", product_ids: Object.keys(HOLDINGS), channels: ["ticker"] }));
    };
    socket.onmessage = (event) => {
      const message = JSON.parse(event.data as string) as { type: string; product_id?: string; price?: string };
      if (message.type !== "ticker" || !message.product_id) return;
      const price = Number(message.price);
      if (!Number.isFinite(price)) return;
      latest.set(message.product_id, price);
      setTicks((n) => n + 1);
      frame ||= requestAnimationFrame(flush);
    };
    socket.onerror = () => setState("Could not reach Coinbase");
    socket.onclose = () => {
      socket = undefined;
      setConnected(false);
      setState((s) => (s === "Live" ? "Disconnected" : s));
    };
  }

  function disconnect() {
    if (socket) socket.onclose = null;
    socket?.close();
    socket = undefined;
    cancelAnimationFrame(frame);
    frame = 0;
    setConnected(false);
    setState("Not connected");
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
          {state()} · {ticks()} messages
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
