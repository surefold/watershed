import { createSignal, onCleanup } from "solid-js";
import { Table } from "@surefold/grid";
import { Grid, createReading } from "@surefold/grid-solid";

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
  const edits = new Table({
    fields: { wiki: "text", title: "text", bot: "bool", bytes: "number" },
    totals: { bytes: "sum" },
  });
  edits.group("wiki"); // a row a wiki, with its edits under it
  const counts = createReading(edits, (t) => ({ edits: t.size, wikis: t.groups().length }));
  const [state, setState] = createSignal("Not connected");
  const [connected, setConnected] = createSignal(false);

  let source: EventSource | undefined;
  let frame = 0;
  let waiting: Edit[] = []; // edits that came since the last frame
  const order: string[] = []; // the keys in the table, oldest first

  // Write the edits that came this frame as one batch: add the new rows, and
  // drop the oldest, so the table keeps the latest KEEP.
  function flush() {
    frame = 0;
    const fresh = waiting.slice(-KEEP);
    waiting = [];
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
    source = new EventSource(STREAM);
    source.onopen = () => setState("Live");
    source.onmessage = (event) => {
      const e = JSON.parse(event.data as string) as Edit;
      if (e.type !== "edit" || e.meta.domain === "canary") return;
      waiting.push(e);
      frame ||= requestAnimationFrame(flush);
    };
    // EventSource reconnects by itself after a dropped connection.
    source.onerror = () => setState(source?.readyState === EventSource.CONNECTING ? "Reconnecting…" : "Could not reach Wikimedia");
  }

  function disconnect() {
    source?.close();
    source = undefined;
    cancelAnimationFrame(frame);
    frame = 0;
    setConnected(false);
    setState("Not connected");
  }

  onCleanup(() => {
    disconnect();
    edits.dispose();
  });

  return (
    <>
      <div class="bar">
        <button onClick={() => (connected() ? disconnect() : connect())}>{connected() ? "Disconnect" : "Connect"}</button>
        <span class="muted">{state()}</span>
        <span class="grow" />
        <span>
          <b>{whole.format(counts().edits)}</b> edits on <b>{counts().wikis}</b> wikis
        </span>
      </div>
      <Grid class="grid" table={edits} keyColumn={{ title: "wiki / edit", width: 220 }} columns={{ title: { width: 280 }, bytes: { decimals: 0, signed: true } }} />
    </>
  );
}
