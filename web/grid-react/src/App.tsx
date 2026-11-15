// The grid page in React: the same book of trades, controls and panels as
// the Solid page (web/grid), drawn by @surefold/grid-react. Everything the
// page shows comes from the table and from what the view reports; the page
// adds nothing up itself.

import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { GroupChart, themes, type AnyTable, type Column, type GridView, type Layout, type Status, type Total } from "@surefold/grid";
import { TradeBook } from "@surefold/grid/demo";
import { Grid } from "@surefold/grid-react";

const whole = new Intl.NumberFormat("en-US");
const two = new Intl.NumberFormat("en-US", { maximumFractionDigits: 2 });

const looks: Record<string, { decimals?: number; width?: number; trend?: boolean; signed?: boolean }> = {
  // As narrow as each column's total allows, so more of them show at once.
  qty: { decimals: 0, width: 108 },
  price: { width: 76 },
  mark: { width: 124, trend: true },
  notional: { width: 154 },
  pnl: { signed: true, width: 124 },
  net: { signed: true, width: 130 },
  side: { width: 60 },
  symbol: { width: 76 },
  desk: { width: 100 },
  trader: { width: 108 },
};
const KEY_COLUMN = { title: "trade", width: 164 };

const THEME_KEY = "watershed-grid-theme";
const WHY_HINT = "Click a cell, a group's total or a total in the bottom row to see what it was worked out from.";
const BLOCK = 2000; // rows per load when loading as you scroll
const MOST = 100000;
// Live from the start, unless the person has asked for less motion.
const LIVE = !matchMedia("(prefers-reduced-motion: reduce)").matches;

/** A share of the graph, with enough places to show a small one. */
function share(part: number, all: number) {
  if (!all) return "";
  const percent = (100 * part) / all;
  return `${percent === 0 ? "0" : percent < 0.01 ? percent.toFixed(4) : percent < 1 ? percent.toFixed(2) : percent.toFixed(0)}% of `;
}

/** Whether the frame this page is in (if it is in one) is on screen. */
function showing() {
  try {
    return !frameElement || frameElement.getClientRects().length > 0;
  } catch {
    return true; // a frame on another site: no way to tell
  }
}

function download(name: string, text: string, type: string) {
  const link = document.createElement("a");
  link.href = URL.createObjectURL(new Blob([text], { type }));
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 1000);
}

export function App() {
  const [book, setBook] = useState<{ id: number; book: TradeBook }>();
  const [lazy, setLazy] = useState(false);
  const [size, setSize] = useState("50000");
  const [theme, setThemeState] = useState(document.documentElement.dataset.wgTheme ?? "");
  const [streaming, setStreamingState] = useState(false);
  const [lookingBack, setLookingBackState] = useState<number | null>(null);
  const [rateLog, setRateLog] = useState(1.3);
  const rate = Math.round(10 ** rateLog);
  const [note, setNote] = useState({ text: "", kind: "" });
  const [stats, setStats] = useState<Status>({});
  const [msShown, setMsShown] = useState<number>();
  const [footer, setFooter] = useState({ rows: 0, filtered: 0, groups: 0, selected: 0 });
  const [range, setRange] = useState<Status["range"]>(null);
  const [more, setMore] = useState({ loading: false, done: false });
  const [columns, setColumns] = useState<Column[]>([]);
  const [group, setGroupState] = useState(-1);
  const [chartName, setChartName] = useState("net");
  const [total, setTotal] = useState({ title: "Total", text: "–", negative: false, line: "" });
  const [why, setWhy] = useState<{ title: string; text: string } | null>(null);
  const [history, setHistory] = useState<[number, number]>([0, 0]);
  const [progress, setProgress] = useState(0);
  const [fee, setFee] = useState(2);
  const [formula, setFormula] = useState({ name: "", source: "", total: "sum" });

  // What the streaming loop and the view's callbacks read, between renders.
  const live = useRef({
    book: undefined as TradeBook | undefined,
    built: 0,
    view: undefined as GridView | undefined,
    chart: undefined as GroupChart | undefined,
    layout: undefined as Layout | undefined, // how the columns were arranged, kept when the rows are rebuilt
    building: false,
    streaming: false,
    lookingBack: null as number | null,
    rate,
    columns: [] as Column[],
    group: -1,
    chartName: "net",
    recent: [] as number[], // the last updates' times, to steady the figure while streaming
    viewReady: undefined as ((view: GridView) => void) | undefined,
    lastTick: 0, // when the last frame was, while streaming
    due: 0, // changes owed, as a fraction builds up between frames
    searchTimer: 0,
  });
  const l = live.current;
  l.rate = rate;
  const chartBox = useRef<HTMLDivElement>(null);
  const say = (text: string, kind = "") => setNote({ text, kind });
  const table = () => l.book!.table;

  // ------------------------------------------------------------- the chart

  const drawChart = useCallback(() => {
    // Its columns can be added as it runs, so it is read as a table of any columns.
    const t: AnyTable | undefined = l.book?.table;
    const info = l.columns.find((c) => c.name === l.chartName && c.total !== "none");
    if (!t || !info || !l.chart) return;
    const decimals = info.total === "count" ? 0 : (looks[info.name]?.decimals ?? 2);
    const number = new Intl.NumberFormat("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
    const format = (v: number) => number.format(v);
    const by = l.columns[l.group]?.name;
    const value = t.total(info.name);
    const trend = t.totalTrend(info.name, 120);
    let line = "";
    if (trend.length > 1) {
      const [low, high] = [Math.min(...trend), Math.max(...trend)];
      const step = 96 / (trend.length - 1);
      const y = (v: number) => (high === low ? 13 : 25 - ((v - low) / (high - low)) * 24).toFixed(1);
      line = trend.map((v, k) => `${k ? "L" : "M"}${(k * step).toFixed(1)} ${y(v)}`).join("");
    }
    setTotal({ title: `${info.total} of ${info.name}`, text: value === null ? "–" : format(value), negative: (value ?? 0) < 0, line });
    l.chart.update({
      title: `${info.total} of ${info.name}${by ? ` by ${by}` : ""}`,
      groups: t.groups().map((g) => ({ key: g.key, rows: g.rows, value: g.totals[info.name] })),
      total: value,
      trend,
      format,
      hint: "Group the rows (the toolbar, or a column's ⋮ menu) to see each group's total as a bar.",
    });
  }, [l]);

  function setColumnsBoth(next: Column[]) {
    l.columns = next;
    setColumns(next);
  }

  // ---------------------------------------------------- what the view says

  function onStatus(s: Status) {
    if (s.selected !== undefined) setFooter((f) => ({ ...f, selected: s.selected! }));
    if (s.range !== undefined) setRange(s.range);
    if (s.loading !== undefined) setMore({ loading: s.loading, done: Boolean(s.done) });
    if (s.epoch === undefined) return;
    if (s.columns) setColumnsBoth(table().columns());
    drawChart();
    if (l.lookingBack === null) setHistory(table().history() ?? [0, 0]);
    setFooter({ rows: s.rows!, filtered: s.filtered!, groups: s.groups!, selected: s.selected! });
    l.recent.push(s.ms!);
    if (l.recent.length > 30) l.recent.shift();
    setMsShown(l.streaming ? l.recent.reduce((a, b) => a + b, 0) / l.recent.length : s.ms);
    setStats(s);
  }

  const onView = useCallback(
    (view: GridView | null) => {
      if (!view) return;
      l.view = view;
      if (l.layout) view.setLayout(l.layout);
      setWhy(null);
      l.viewReady?.(view);
      l.viewReady = undefined;
    },
    [l],
  );

  // -------------------------------------------------------------- building

  async function build(chosen: string) {
    if (l.building) return;
    l.building = true;
    setStreaming(false);
    if (l.view) l.layout = l.view.getLayout();
    lookBackState(null);
    const old = l.book;
    const next = new TradeBook();
    next.table.setValue("fee_bps", fee);
    const scrolling = chosen === "scroll";
    if (scrolling) next.streamingRows();
    l.book = next;
    l.group = -1; // a grid built again starts with its rows not grouped
    setGroupState(-1);
    setColumnsBoth(next.table.columns());
    setLazy(scrolling);
    // <Grid> draws the new table once React renders, and its ref gives the view.
    const ready = new Promise<GridView>((resolve) => (l.viewReady = resolve));
    setBook({ id: ++l.built, book: next });
    const view = await ready;
    old?.table.dispose();
    if (scrolling) {
      await view.fetchMore();
    } else {
      const rows = Number(chosen);
      const chunk = 5000;
      for (let made = 0; made < rows; made += chunk) {
        next.fill(Math.min(chunk, rows - made));
        next.table.flush();
        setProgress(Math.round((100 * (made + chunk)) / rows));
        say(`Building ${whole.format(Math.min(made + chunk, rows))} of ${whole.format(rows)} rows…`);
        await new Promise((done) => setTimeout(done));
      }
      setProgress(0);
      say("");
    }
    l.building = false;
    if (LIVE) setStreaming(true);
  }

  async function loadMore() {
    const mine = l.book;
    // Stands in for a server: a short wait, then the next block of rows.
    await new Promise((done) => setTimeout(done, 160));
    if (l.book !== mine || !mine) return false;
    mine.fill(BLOCK);
    return mine.table.size < MOST;
  }

  // ------------------------------------------------------------- streaming

  function tick(now: number) {
    if (!l.streaming || l.building) return;
    // Not showing (its tab is not the one open): rest, and look again shortly.
    if (!showing()) return void setTimeout(() => requestAnimationFrame(tick), 400);
    // Changes come due at the chosen rate; a slow rate skips frames, so each
    // change can be seen, and a fast one sends many at once.
    const elapsed = l.lastTick ? Math.min(now - l.lastTick, 250) : 16;
    l.lastTick = now;
    l.due += (l.rate * elapsed) / 1000;
    const changes = Math.floor(l.due);
    if (changes > 0) {
      l.due -= changes;
      l.book?.tick(changes);
    }
    requestAnimationFrame(tick);
  }

  function setStreaming(on: boolean) {
    if (on && l.lookingBack !== null) lookBack(null);
    const was = l.streaming;
    l.streaming = on;
    setStreamingState(on);
    l.lastTick = 0;
    if (on && !was) requestAnimationFrame(tick);
  }

  function lookBackState(epoch: number | null) {
    l.lookingBack = epoch;
    setLookingBackState(epoch);
  }

  function lookBack(epoch: number | null) {
    if (epoch !== null) setStreaming(false);
    lookBackState(epoch);
    l.view?.setAsOf(epoch);
    say(epoch !== null ? "Showing the grid as it was. Go back to now to edit or stream." : "");
  }

  // -------------------------------------------------------------- controls

  function setTheme(id: string) {
    const chosen = themes.some((t) => t.id === id) ? id : "";
    setThemeState(chosen);
    if (chosen) document.documentElement.dataset.wgTheme = chosen;
    else delete document.documentElement.dataset.wgTheme;
    try {
      if (chosen) localStorage.setItem(THEME_KEY, chosen);
      else localStorage.removeItem(THEME_KEY);
    } catch {
      // No storage (a private window): the choice lasts as long as the page.
    }
  }

  function setGroup(col: number) {
    say(col >= 0 ? "Grouping…" : "");
    // Let "Grouping…" show: the first time, each row joins its group's totals.
    setTimeout(() => {
      l.view?.setGroup(col);
      say("");
    });
  }

  function search(text: string) {
    clearTimeout(l.searchTimer);
    l.searchTimer = window.setTimeout(() => l.view?.setQuickFilter(text), 140);
  }

  function addColumn() {
    const [name, source] = [formula.name.trim(), formula.source.trim()];
    if (!name || !source) return say("Give the column a name and a formula, such as: bps = pnl / notional * 10000", "bad");
    try {
      table().addFormula(name, source, formula.total as Total);
      setFormula({ ...formula, name: "", source: "" });
      say(`Added ${name}.`, "good");
    } catch (error) {
      say((error as Error).message, "bad");
    }
  }

  function removeSelected() {
    const count = l.view?.selectedKeys().length ?? 0;
    if (!count) return say("Click a trade in the first column to select its row first. Shift-click selects a run of rows.", "bad");
    l.view!.removeSelected();
    say(`Deleted ${whole.format(count)} row${count === 1 ? "" : "s"}.`, "good");
  }

  function check() {
    const started = performance.now();
    const wrong = table().check();
    const ms = (performance.now() - started).toFixed(0);
    if (wrong) say(wrong, "bad");
    else say(`Every node matches a full recalculation (${ms} ms).`, "good");
  }

  // Once, on the first render: the chart, and the first book of trades.
  // (Strict mode runs this twice in development; the second finds it done.)
  useEffect(() => {
    if (!l.chart) {
      l.chart = new GroupChart(chartBox.current!, { onPick: (key) => l.view?.revealGroup(key) });
      void build(size);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const pillText = streaming ? `Streaming · ${whole.format(rate)}/s` : lookingBack !== null ? "Looking back" : "Paused";
  const viewText = stats.rebuilt
    ? "The view was filtered and sorted again."
    : stats.patched
      ? "Only the rows that changed were moved in the view."
      : "The view stood: only cells and totals changed.";
  const groupable = columns.flatMap((c, k) => (c.groupable ? [{ name: c.name, col: k }] : []));
  const totalled = columns.filter((c) => c.total !== "none");
  const pillSlot = document.getElementById("pill-slot");

  return (
    <>
      {/* The header is in the page's HTML; only its stream switch is drawn here. */}
      {pillSlot &&
        createPortal(
          <button
            type="button"
            className={`pill${streaming ? " on" : ""}`}
            aria-pressed={streaming}
            title="Start or pause the stream of price changes"
            onClick={() => setStreaming(!l.streaming)}
          >
            <span>{pillText}</span>
            <span className="act">{streaming ? "Pause" : "Stream"}</span>
          </button>,
          pillSlot,
        )}

      <dl className="stats" aria-label="Last update">
        <div className="stat">
          <dt>Inputs changed</dt>
          <dd>{stats.inputsChanged === undefined ? "–" : whole.format(stats.inputsChanged)}</dd>
          <p>in the last update</p>
        </div>
        <div className="stat">
          <dt>Nodes recalculated</dt>
          <dd className="live">{stats.recalculated === undefined ? "–" : whole.format(stats.recalculated)}</dd>
          <p>{stats.nodes ? `${share(stats.recalculated!, stats.nodes)}${whole.format(stats.nodes)} nodes in the graph` : " "}</p>
        </div>
        <div className="stat">
          <dt>Update and redraw</dt>
          <dd>
            {msShown === undefined ? (
              "–"
            ) : (
              <>
                {msShown < 10 ? msShown.toFixed(2) : msShown.toFixed(1)} <small>ms</small>
              </>
            )}
          </dd>
          <p>{stats.epoch === undefined ? " " : viewText}</p>
        </div>
        <div className="stat">
          <dt>{total.title}</dt>
          <div className="with-line">
            <dd className={total.negative ? "neg" : undefined}>{total.text}</dd>
            <svg className="t-line" viewBox="0 0 96 26" preserveAspectRatio="none" aria-hidden="true" style={{ visibility: total.line ? "visible" : "hidden" }}>
              <path d={total.line} />
            </svg>
          </div>
          <p>
            across {whole.format(footer.filtered)} {footer.filtered === footer.rows ? "" : "shown "}rows
          </p>
        </div>
      </dl>

      <section className="tools" aria-label="Grid controls">
        <div className="bar">
          <input type="search" className="search" placeholder="Search keys and text…" aria-label="Search keys and text" onChange={(e) => search(e.target.value)} />
          <label>
            Group by{" "}
            <select value={String(group)} onChange={(e) => setGroup(Number(e.target.value))}>
              <option value="-1">nothing</option>
              {groupable.map((c) => (
                <option key={c.col} value={String(c.col)}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
          <button disabled={group < 0} onClick={() => l.view?.expandAll(true)}>
            Open all
          </button>
          <button disabled={group < 0} onClick={() => l.view?.expandAll(false)}>
            Close all
          </button>
          <span className="sep" />
          <label>
            Rows{" "}
            <select
              value={size}
              onChange={(e) => {
                setSize(e.target.value);
                void build(e.target.value);
              }}
            >
              <option value="10000">10,000</option>
              <option value="50000">50,000</option>
              <option value="100000">100,000</option>
              <option value="scroll">load as you scroll</option>
            </select>
          </label>
          <label>
            Theme{" "}
            <select value={theme} onChange={(e) => setTheme(e.target.value)}>
              {themes.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                </option>
              ))}
            </select>
          </label>
          <span className="grow" />
          <button onClick={removeSelected}>Delete selected</button>
          <button onClick={() => download("watershed-grid.csv", table().csv(), "text/csv")}>Export CSV</button>
          <button className="check" title="Recalculates every node from scratch and compares with the incremental values." onClick={check}>
            Check against full recalculation
          </button>
        </div>
        <div className="bar">
          <button className="stream" aria-pressed={streaming} onClick={() => setStreaming(!l.streaming)}>
            {streaming ? "Pause prices" : "Stream prices"}
          </button>
          <label className="rate-control" title="How many trades' marks move each second. Slow enough to follow by default; drag right for thousands.">
            price changes a second{" "}
            <input type="range" min="0" max="5.3" step="0.01" value={rateLog} onChange={(e) => setRateLog(Number(e.target.value))} />{" "}
            <output>{whole.format(rate)}</output>
          </label>
          <span className="sep" />
          <label title="A named value used by the net column. Changing it recalculates net in every row.">
            fee_bps{" "}
            <input
              type="number"
              value={fee}
              min="0"
              max="100"
              step="0.5"
              onChange={(e) => {
                const value = Number(e.target.value);
                if (!Number.isFinite(value)) return;
                setFee(value);
                table().setValue("fee_bps", value);
              }}
            />
          </label>
          <span className="grow" />
          <label title="Drag back to see the grid as it was after an earlier update: values, totals, order and filters.">
            Time{" "}
            <input
              type="range"
              min={history[0]}
              max={history[1]}
              value={lookingBack ?? history[1]}
              step="1"
              onChange={(e) => {
                const epoch = Number(e.target.value);
                lookBack(epoch >= history[1] ? null : epoch);
              }}
            />{" "}
            <output>{lookingBack === null ? "now" : `update ${whole.format(lookingBack)}`}</output>
          </label>
          <button disabled={lookingBack === null} onClick={() => lookBack(null)}>
            Back to now
          </button>
        </div>
        <div className="bar formula">
          <span className="fx" aria-hidden="true">
            ƒ
          </span>
          <label>
            New column{" "}
            <input
              type="text"
              className="code formula-name"
              placeholder="name"
              spellCheck={false}
              aria-label="New column name"
              value={formula.name}
              onChange={(e) => setFormula({ ...formula, name: e.target.value })}
              onKeyDown={(e) => e.key === "Enter" && addColumn()}
            />
          </label>
          <label>
            ={" "}
            <input
              type="text"
              className="code formula-src"
              placeholder="pnl / notional * 10000"
              spellCheck={false}
              aria-label="New column formula"
              value={formula.source}
              onChange={(e) => setFormula({ ...formula, source: e.target.value })}
              onKeyDown={(e) => e.key === "Enter" && addColumn()}
            />
          </label>
          <label>
            total{" "}
            <select aria-label="New column total" value={formula.total} onChange={(e) => setFormula({ ...formula, total: e.target.value })}>
              {["sum", "avg", "min", "max", "count", "none"].map((t) => (
                <option key={t} value={t}>
                  {t}
                </option>
              ))}
            </select>
          </label>
          <button className="primary" onClick={addColumn}>
            Add column
          </button>
          <span className={`note ${note.kind}`} role="status">
            {note.text}
          </span>
        </div>
        <div className="progress" style={{ width: `${progress}%` }} />
      </section>

      <main>
        {book && (
          <Grid
            key={book.id}
            ref={onView}
            className="grid"
            table={book.book.table}
            keyColumn={KEY_COLUMN}
            label="Trades"
            theme={theme}
            columns={looks}
            loadMore={lazy ? loadMore : undefined}
            onStatus={onStatus}
            onFocus={() => setWhy(l.view?.explainFocus() ?? null)}
            onError={(m) => say(m, "bad")}
            onNote={(m) => say(m, "good")}
            onGroup={(col) => {
              l.group = col;
              setGroupState(col);
              drawChart();
            }}
          />
        )}
        <aside>
          <div className="card chart">
            <div className="card-head">
              <h2>Totals by group</h2>
              <select
                aria-label="Column to chart"
                value={chartName}
                onChange={(e) => {
                  l.chartName = e.target.value;
                  setChartName(e.target.value);
                  drawChart();
                }}
              >
                {totalled.map((c) => (
                  <option key={c.name} value={c.name}>{`${c.total} of ${c.name}`}</option>
                ))}
              </select>
            </div>
            <div ref={chartBox} className="chart-box" />
          </div>
          <div className="card">
            <h2>{why ? `Why · ${why.title}` : "Why this value"}</h2>
            <pre className={why ? "why" : "why hint"}>{why?.text ?? WHY_HINT}</pre>
          </div>
        </aside>
      </main>

      <footer>
        <span>
          <b>{whole.format(footer.rows)}</b> rows
        </span>
        <span>
          <b>{whole.format(footer.filtered)}</b> shown
        </span>
        {footer.groups > 0 && (
          <span>
            <b>{whole.format(footer.groups)}</b> groups
          </span>
        )}
        <span>
          <b>{whole.format(footer.selected)}</b> selected
        </span>
        {range && (
          <span>
            <b>{whole.format(range.cells)}</b> cells{" "}
            {range.numbers ? (
              <>
                <b>{two.format(range.sum)}</b> sum <b>{two.format(range.avg)}</b> avg <b>{two.format(range.min)}</b> min <b>{two.format(range.max)}</b> max
              </>
            ) : null}
          </span>
        )}
        {lazy && (
          <span>
            {more.loading ? "Loading more rows…" : more.done ? "Every row is loaded." : "More rows load as you scroll."}{" "}
            {!more.loading && !more.done && (
              <button className="more" onClick={() => l.view?.fetchMore()}>
                Load more
              </button>
            )}
          </span>
        )}
        <ul
          className="keys"
          title="Click a header to sort (Shift adds a sort); drag it to move. Drag over cells, or Shift+arrows, for a block: copy, paste, ⌘Enter fills. Click a trade to select its row."
        >
          <li>
            <kbd>Click</kbd> a header sorts, <kbd>Shift</kbd> adds
          </li>
          <li>
            <kbd>⋮</kbd> pin, hide, group
          </li>
          <li>
            <kbd>&gt;100</kbd> <kbd>5..10</kbd> <kbd>=Rates</kbd> filter
          </li>
          <li>
            <kbd>Shift</kbd>+arrows pick a block
          </li>
          <li>
            <kbd>⌘Enter</kbd> fills
          </li>
          <li>
            <kbd>⌘Z</kbd> undoes
          </li>
        </ul>
      </footer>
    </>
  );
}
