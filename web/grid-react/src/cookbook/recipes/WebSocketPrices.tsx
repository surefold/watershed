import { useEffect, useRef, useState } from "react";
import { Table } from "@surefold/grid";
import { Grid, useTable, useTotal } from "@surefold/grid-react";

// Coinbase's public ticker: a message each time a coin trades. No key needed.
const FEED = "wss://ws-feed.exchange.coinbase.com";
const HOLDINGS: Record<string, number> = { "BTC-USD": 0.5, "ETH-USD": 8, "SOL-USD": 120, "DOGE-USD": 40000, "LTC-USD": 60, "LINK-USD": 400 };
const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

export function WebSocketPrices() {
  const coins = useTable(
    () =>
      new Table({
        fields: { units: "number", open: "number", price: "number" },
        formulas: {
          value: "units * price",
          gain: "units * (price - open)", // since this page connected
        },
        totals: { value: "sum", gain: "sum" },
        history: 120, // for each price's trend in its cell
      }),
  );
  const gain = useTotal(coins, "gain");
  const [state, setState] = useState("Not connected");
  const [ticks, setTicks] = useState(0);
  const [connected, setConnected] = useState(false);

  // The connection and what has come since the last frame live in a ref, not in state:
  // they change with every message, and rendering does not need them.
  const live = useRef({ socket: undefined as WebSocket | undefined, frame: 0, latest: new Map<string, number>(), known: new Set<string>() });

  // Many messages can come in a frame: keep the latest price of each coin, and
  // write them all to the table once a frame, as one batch (one recalculation).
  function flush() {
    const { latest, known } = live.current;
    live.current.frame = 0;
    coins.batch(() => {
      for (const [coin, price] of latest) {
        if (known.has(coin)) coins.set(coin, "price", price);
        else coins.put(coin, { units: HOLDINGS[coin], open: price, price }); // its first price: add its row
        known.add(coin);
      }
    });
    latest.clear();
  }

  function connect() {
    setState("Connecting…");
    setConnected(true);
    const socket = (live.current.socket = new WebSocket(FEED));
    socket.onopen = () => {
      setState("Live");
      socket.send(JSON.stringify({ type: "subscribe", product_ids: Object.keys(HOLDINGS), channels: ["ticker"] }));
    };
    socket.onmessage = (event) => {
      const message = JSON.parse(event.data as string) as { type: string; product_id?: string; price?: string };
      if (message.type !== "ticker" || !message.product_id) return;
      const price = Number(message.price);
      if (!Number.isFinite(price)) return;
      live.current.latest.set(message.product_id, price);
      setTicks((n) => n + 1);
      live.current.frame ||= requestAnimationFrame(flush);
    };
    socket.onerror = () => setState("Could not reach Coinbase");
    socket.onclose = () => {
      live.current.socket = undefined;
      setConnected(false);
      setState((s) => (s === "Live" ? "Disconnected" : s));
    };
  }

  function stop() {
    const { socket, frame } = live.current;
    if (socket) socket.onclose = null;
    socket?.close();
    cancelAnimationFrame(frame);
    live.current.socket = undefined;
    live.current.frame = 0;
  }

  function disconnect() {
    stop();
    setConnected(false);
    setState("Not connected");
  }

  // Close the connection when the component goes.
  useEffect(() => stop, []);

  return (
    <>
      <div className="bar">
        <button onClick={() => (connected ? disconnect() : connect())}>{connected ? "Disconnect" : "Connect"}</button>
        <span className="muted">
          {state} · {ticks} messages
        </span>
        <span className="grow" />
        <span>
          Gain since connecting: <b>{gain === null ? "–" : money.format(gain)}</b>
        </span>
      </div>
      <Grid
        className="grid"
        table={coins}
        keyColumn={{ title: "coin", width: 110 }}
        columns={{ units: { decimals: 2 }, open: { decimals: 4 }, price: { decimals: 4, trend: true, width: 150 }, gain: { signed: true } }}
      />
    </>
  );
}
