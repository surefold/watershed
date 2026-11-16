import { useEffect, useRef, useState } from "react";
import { Table } from "@surefold/grid";
import { Grid, useReading, useTable } from "@surefold/grid-react";

// Every edit to a Wikimedia wiki, as server-sent events. No key needed.
const STREAM = "https://stream.wikimedia.org/v2/stream/recentchange";
const KEEP = 2000; // the latest edits kept in the table
const whole = new Intl.NumberFormat("en-US");

interface Edit {
  type: string;
  server_name: string;
  title: string;
  bot: boolean;
  length?: { old?: number; new?: number };
  meta: { id: string; domain?: string };
}

export function ServerSentEvents() {
  const edits = useTable(() => {
    const t = new Table({
      fields: { wiki: "text", title: "text", bot: "bool", bytes: "number" },
      totals: { bytes: "sum" },
    });
    t.group("wiki"); // a row a wiki, with its edits under it
    return t;
  });
  const count = useReading(edits, (t) => t.size);
  const wikis = useReading(edits, (t) => t.groups().length);
  const [state, setState] = useState("Not connected");
  const [connected, setConnected] = useState(false);

  // The connection and what has come since the last frame live in a ref, not in state:
  // they change with every message, and rendering does not need them.
  const live = useRef({ source: undefined as EventSource | undefined, frame: 0, waiting: [] as Edit[], order: [] as string[] });

  // Write the edits that came this frame as one batch: add the new rows, and
  // drop the oldest, so the table keeps the latest KEEP.
  function flush() {
    const { order } = live.current;
    const fresh = live.current.waiting.slice(-KEEP);
    live.current.waiting = [];
    live.current.frame = 0;
    edits.batch(() => {
      for (const e of fresh) {
        const bytes = (e.length?.new ?? 0) - (e.length?.old ?? 0);
        edits.put(e.meta.id, { wiki: e.server_name, title: e.title, bot: e.bot, bytes });
        order.push(e.meta.id);
      }
      for (const key of order.splice(0, Math.max(0, order.length - KEEP))) edits.drop(key);
    });
  }

  function connect() {
    setState("Connecting…");
    setConnected(true);
    const source = (live.current.source = new EventSource(STREAM));
    source.onopen = () => setState("Live");
    source.onmessage = (event) => {
      const e = JSON.parse(event.data as string) as Edit;
      if (e.type !== "edit" || e.meta.domain === "canary") return;
      live.current.waiting.push(e);
      live.current.frame ||= requestAnimationFrame(flush);
    };
    // EventSource reconnects by itself after a dropped connection.
    source.onerror = () => setState(source.readyState === EventSource.CONNECTING ? "Reconnecting…" : "Could not reach Wikimedia");
  }

  function stop() {
    live.current.source?.close();
    cancelAnimationFrame(live.current.frame);
    live.current.source = undefined;
    live.current.frame = 0;
  }

  function disconnect() {
    stop();
    setConnected(false);
    setState("Not connected");
  }

  // Close the stream when the component goes.
  useEffect(() => stop, []);

  return (
    <>
      <div className="bar">
        <button onClick={() => (connected ? disconnect() : connect())}>{connected ? "Disconnect" : "Connect"}</button>
        <span className="muted">{state}</span>
        <span className="grow" />
        <span>
          <b>{whole.format(count)}</b> edits on <b>{wikis}</b> wikis
        </span>
      </div>
      <Grid className="grid" table={edits} keyColumn={{ title: "wiki / edit", width: 220 }} columns={{ title: { width: 280 }, bytes: { decimals: 0, signed: true } }} />
    </>
  );
}
