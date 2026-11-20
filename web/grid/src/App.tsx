// The site's grid page: a book of trades whose marks stream, drawn by
// @surefold/grid-solid. Everything the page shows comes from the table and
// from what the view reports; the page adds nothing up itself.

import { createSignal, flush, For, onCleanup, onSettled, Show } from "solid-js";
import { render } from "@solidjs/web";
import { GroupChart, themes, type Column, type Focus, type GridView, type Layout, type Status, type AnyGridTable, type ViewState } from "@surefold/grid";
import { WorkerTradeBook } from "@surefold/grid/demo";
import { Grid } from "@surefold/grid-solid";

const whole = new Intl.NumberFormat("en-US");
const two = new Intl.NumberFormat("en-US", { maximumFractionDigits: 2 });

const looks = {
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
  // The rows live in a Web Worker: building them, streaming prices and every
  // recalculation happen there, and the page only draws.
  const [book, setBook] = createSignal<WorkerTradeBook>();
  const [lazy, setLazy] = createSignal(false);
  const [size, setSize] = createSignal("50000");
  const [theme, setThemeSignal] = createSignal(document.documentElement.dataset.wgTheme ?? "");
  const [streaming, setStreamingSignal] = createSignal(false);
  const [lookingBack, setLookingBack] = createSignal<number | null>(null);
  const [rateLog, setRateLog] = createSignal(1.3);
  const rate = () => Math.round(10 ** rateLog());
  const [note, setNoteSignal] = createSignal({ text: "", kind: "" });
  const [stats, setStats] = createSignal<Status>({});
  const [msShown, setMsShown] = createSignal<number>();
  const [footer, setFooter] = createSignal({ rows: 0, filtered: 0, groups: 0, selected: 0 });
  const [range, setRange] = createSignal<Status["range"]>(null);
  const [more, setMore] = createSignal<{ loading: boolean; done: boolean }>({ loading: false, done: false });
  const [columns, setColumns] = createSignal<Column[]>([]);
  const [group, setGroupSignal] = createSignal(-1);
  const [chartName, setChartName] = createSignal("net");
  const [total, setTotal] = createSignal({ title: "Total", text: "–", negative: false, line: "" });
  const [why, setWhy] = createSignal<{ title: string; text: string } | null>(null);
  const [history, setHistory] = createSignal<[number, number]>([0, 0]);
  const [progress, setProgress] = createSignal(0);
  const [fee, setFee] = createSignal(2);
  // How the table's rows are shaped, as the table says: set from the grid or from code.
  const [shape, setShape] = createSignal<ViewState>({ sort: [], filters: {}, search: "", group: null });
  let searchBox!: HTMLInputElement;

  let view: GridView | undefined;
  let chart: GroupChart | undefined;
  let chartBox!: HTMLDivElement;
  let layout: Layout | undefined; // how the columns were arranged, kept when the rows are rebuilt
  let building = false;
  const recent: number[] = []; // the last updates' times, to steady the figure while streaming

  const say = (text: string, kind = "") => setNoteSignal({ text, kind });
  const table = () => book()!.table;

  // ------------------------------------------------------------- the chart

  function drawChart() {
    // Its columns can be added as it runs, so it is read as a table of any columns.
    const t: AnyGridTable | undefined = book()?.table;
    if (!t || !chart) return;
    // Asked of the table, not the signals: a signal set a moment ago still reads as it was.
    const info = t.columns().find((c) => c.name === chartName() && c.total !== "none");
    if (!info) return;
    const decimals = info.total === "count" ? 0 : ((looks as Record<string, { decimals?: number }>)[info.name]?.decimals ?? 2);
    const number = new Intl.NumberFormat("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
    const format = (v: number) => number.format(v);
    const by = t.viewState().group;
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
    chart.update({
      title: `${info.total} of ${info.name}${by ? ` by ${by}` : ""}`,
      groups: t.groups().map((g) => ({ key: g.key, rows: g.rows, value: g.totals[info.name] })),
      total: value,
      trend,
      format,
      hint: "Group the rows (the toolbar, or a column's ⋮ menu) to see each group's total as a bar.",
    });
  }

  // ---------------------------------------------------- what the view says

  function onStatus(s: Status) {
    if (s.selected !== undefined) setFooter((f) => ({ ...f, selected: s.selected! }));
    if (s.range !== undefined) setRange(s.range);
    if (s.loading !== undefined) setMore({ loading: s.loading, done: Boolean(s.done) });
    if (s.epoch === undefined) return;
    if (s.columns) setColumns(table().columns());
    if (s.view || s.columns) {
      const state = table().viewState();
      setShape(state);
      // The search box shows the table's search, unless it is being typed in.
      if (searchBox && searchBox !== document.activeElement) searchBox.value = state.search;
    }
    drawChart();
    if (lookingBack() === null) setHistory(table().history() ?? [0, 0]);
    setFooter({ rows: s.rows!, filtered: s.filtered!, groups: s.groups!, selected: s.selected! });
    recent.push(s.ms!);
    if (recent.length > 30) recent.shift();
    setMsShown(streaming() ? recent.reduce((a, b) => a + b, 0) / recent.length : s.ms);
    setStats(s);
  }

  function onFocus(_focus: Focus | null) {
    setWhy(view?.explainFocus() ?? null);
  }

  function onView(v: GridView) {
    view = v;
    if (layout) v.setLayout(layout);
    setWhy(null);
  }

  // -------------------------------------------------------------- building

  async function build(chosen: string) {
    if (building) return;
    building = true;
    setStreaming(false);
    if (view) layout = view.getLayout();
    setLookingBack(null);
    const old = book();
    const next = await WorkerTradeBook.create();
    void next.table.setValue("fee_bps", fee());
    const scrolling = chosen === "scroll";
    if (scrolling) void next.streamingRows();
    setLazy(scrolling);
    loaded = 0;
    setGroupSignal(-1); // a grid built again starts with its rows not grouped
    setColumns(next.table.columns());
    setBook(next);
    flush(); // <Grid> draws the new table, and calls onView
    old?.table.dispose();
    if (scrolling) {
      await view!.fetchMore();
    } else {
      const rows = Number(chosen);
      const chunk = 5000;
      for (let made = 0; made < rows; made += chunk) {
        // The worker builds them; the page stays free to scroll and click.
        await next.fill(Math.min(chunk, rows - made));
        setProgress(Math.round((100 * (made + chunk)) / rows));
        say(`Building ${whole.format(Math.min(made + chunk, rows))} of ${whole.format(rows)} rows…`);
        await new Promise((done) => setTimeout(done));
      }
      setProgress(0);
      say("");
    }
    building = false;
    if (LIVE) setStreaming(true);
  }

  let loaded = 0; // rows loaded as the person scrolled, in the book being shown
  async function loadMore() {
    const mine = book();
    // Stands in for a server: a short wait, then the next block of rows.
    await new Promise((done) => setTimeout(done, 160));
    if (book() !== mine || !mine) return false;
    await mine.fill(BLOCK);
    loaded += BLOCK;
    return loaded < MOST;
  }

  // ------------------------------------------------------------- streaming

  let lastTick = 0; // when the last frame was, while streaming
  let due = 0; // changes owed, as a fraction builds up between frames

  function tick(now: number) {
    if (!streaming() || building) return;
    // Not showing (its tab is not the one open): rest, and look again shortly.
    if (!showing()) return void setTimeout(() => requestAnimationFrame(tick), 400);
    // Changes come due at the chosen rate; a slow rate skips frames, so each
    // change can be seen, and a fast one sends many at once.
    const elapsed = lastTick ? Math.min(now - lastTick, 250) : 16;
    lastTick = now;
    due += (rate() * elapsed) / 1000;
    const changes = Math.floor(due);
    if (changes > 0) {
      due -= changes;
      void book()?.tick(changes);
    }
    requestAnimationFrame(tick);
  }

  function setStreaming(on: boolean) {
    if (on && lookingBack() !== null) lookBack(null);
    const was = streaming();
    setStreamingSignal(on);
    lastTick = 0;
    if (on && !was) requestAnimationFrame(tick);
  }

  function lookBack(epoch: number | null) {
    if (epoch !== null) setStreaming(false);
    setLookingBack(epoch);
    flush(); // onStatus reads it
    view?.setAsOf(epoch);
    say(epoch !== null ? "Showing the grid as it was. Go back to now to edit or stream." : "");
  }

  // -------------------------------------------------------------- controls

  function setTheme(id: string) {
    const chosen = themes.some((t) => t.id === id) ? id : "";
    setThemeSignal(chosen);
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
      view?.setGroup(col);
      say("");
    });
  }

  let searchTimer = 0;
  function search(text: string) {
    clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => view?.setQuickFilter(text), 140);
  }

  let formulaName!: HTMLInputElement;
  let formula!: HTMLInputElement;
  let formulaTotal!: HTMLSelectElement;
  function addColumn() {
    const [name, source] = [formulaName.value.trim(), formula.value.trim()];
    if (!name || !source) return say("Give the column a name and a formula, such as: bps = pnl / notional * 10000", "bad");
    try {
      table()
        .addFormula(name, source, formulaTotal.value as "sum")
        .then(() => {
          formulaName.value = formula.value = "";
          say(`Added ${name}.`, "good");
        })
        .catch((error: Error) => say(error.message, "bad"));
    } catch (error) {
      say((error as Error).message, "bad");
    }
  }

  function removeSelected() {
    const count = view?.selectedKeys().length ?? 0;
    if (!count) return say("Click a trade in the first column to select its row first. Shift-click selects a run of rows.", "bad");
    view!.removeSelected();
    say(`Deleted ${whole.format(count)} row${count === 1 ? "" : "s"}.`, "good");
  }

  /** The table's sort, filters, search and grouping, in words. */
  const shapeText = () => {
    const v = shape();
    const parts = [
      ...v.sort.map((s) => `sorted by ${s.column} ${s.desc ? "↓" : "↑"}`),
      ...Object.entries(v.filters).map(([column, text]) => `${column} ${/^[<>=]|\.\./.test(text) ? text : `has “${text}”`}`),
      ...(v.search ? [`search “${v.search}”`] : []),
      ...(v.group ? [`grouped by ${v.group}`] : []),
    ];
    return parts.length ? parts.join(" · ") : "as loaded: no sort, filter or grouping";
  };

  /** Shapes the rows from code, through the table, as an app would; the grid follows. */
  async function shapeFromCode(preset: "losses" | "rates" | "clear") {
    const t = table();
    try {
      if (preset === "losses") await t.sort([{ column: "net", desc: false }]);
      if (preset === "rates") {
        await t.filter("desk", "=Rates");
        await t.group("trader");
      }
      if (preset === "clear") {
        await t.sort([]);
        for (const column of Object.keys(t.viewState().filters)) await t.filter(column as "desk", "");
        await t.search("");
        await t.group(null);
      }
    } catch (error) {
      say((error as Error).message, "bad");
    }
  }

  async function check() {
    const started = performance.now();
    const wrong = await table().check();
    const ms = (performance.now() - started).toFixed(0);
    if (wrong) say(wrong, "bad");
    else say(`Every node matches a full recalculation (${ms} ms).`, "good");
  }

  const pillText = () => (streaming() ? `Streaming · ${whole.format(rate())}/s` : lookingBack() !== null ? "Looking back" : "Paused");
  // The header is in the page's HTML, so it is there before any script runs;
  // only its stream switch is drawn here.
  const slot = document.getElementById("pill-slot");
  if (slot) {
    onCleanup(
      render(
        () => (
          <button
            type="button"
            class={["pill", { on: streaming() }]}
            aria-pressed={streaming() ? "true" : "false"}
            title="Start or pause the stream of price changes"
            onClick={() => setStreaming(!streaming())}
          >
            <span>{pillText()}</span>
            <span class="act">{streaming() ? "Pause" : "Stream"}</span>
          </button>
        ),
        slot,
      ),
    );
  }
  onSettled(() => {
    chart = new GroupChart(chartBox, { onPick: (key) => view?.revealGroup(key) });
    void build(size());
  });

  const viewText = () => {
    const s = stats();
    return s.rebuilt ? "The view was filtered and sorted again." : s.patched ? "Only the rows that changed were moved in the view." : "The view stood: only cells and totals changed.";
  };
  const groupable = () => columns().flatMap((c, k) => (c.groupable ? [{ name: c.name, col: k }] : []));
  const totalled = () => columns().filter((c) => c.total !== "none");

  return (
    <>
      <dl class="stats" aria-label="Last update">
        <div class="stat">
          <dt>Inputs changed</dt>
          <dd>{stats().inputsChanged === undefined ? "–" : whole.format(stats().inputsChanged!)}</dd>
          <p>in the last update</p>
        </div>
        <div class="stat">
          <dt>Nodes recalculated</dt>
          <dd class="live">{stats().recalculated === undefined ? "–" : whole.format(stats().recalculated!)}</dd>
          <p>{stats().nodes ? `${share(stats().recalculated!, stats().nodes!)}${whole.format(stats().nodes!)} nodes in the graph` : " "}</p>
        </div>
        <div class="stat">
          <dt>Update and redraw</dt>
          <dd>
            <Show when={msShown() !== undefined} fallback="–">
              {msShown()! < 10 ? msShown()!.toFixed(2) : msShown()!.toFixed(1)} <small>ms</small>
            </Show>
          </dd>
          <p>{stats().epoch === undefined ? " " : viewText()}</p>
        </div>
        <div class="stat">
          <dt>{total().title}</dt>
          <div class="with-line">
            <dd class={{ neg: total().negative }}>{total().text}</dd>
            <svg class="t-line" viewBox="0 0 96 26" preserveAspectRatio="none" aria-hidden="true" style={{ visibility: total().line ? "visible" : "hidden" }}>
              <path d={total().line} />
            </svg>
          </div>
          <p>
            across {whole.format(footer().filtered)} {footer().filtered === footer().rows ? "" : "shown "}rows
          </p>
        </div>
      </dl>

      <section class="tools" aria-label="Grid controls">
        <div class="bar">
          <input
            ref={searchBox}
            type="search"
            class="search"
            placeholder="Search keys and text…"
            aria-label="Search keys and text"
            onInput={(e) => search(e.currentTarget.value)}
          />
          <label>
            Group by{" "}
            <select value={String(group())} onChange={(e) => setGroup(Number(e.currentTarget.value))}>
              <option value="-1">nothing</option>
              <For each={groupable()} keyed={(c) => c.name}>{(c) => <option value={String(c().col)}>{c().name}</option>}</For>
            </select>
          </label>
          <button disabled={group() < 0} onClick={() => view?.expandAll(true)}>
            Open all
          </button>
          <button disabled={group() < 0} onClick={() => view?.expandAll(false)}>
            Close all
          </button>
          <span class="sep" />
          <label>
            Rows{" "}
            <select
              value={size()}
              onChange={(e) => {
                setSize(e.currentTarget.value);
                void build(e.currentTarget.value);
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
            <select value={theme()} onChange={(e) => setTheme(e.currentTarget.value)}>
              <For each={themes}>{(t) => <option value={t.id}>{t.name}</option>}</For>
            </select>
          </label>
          <span class="grow" />
          <button onClick={removeSelected}>Delete selected</button>
          <button onClick={async () => download("watershed-grid.csv", await table().csv(), "text/csv")}>Export CSV</button>
          <button class="check" title="Recalculates every node from scratch and compares with the incremental values." onClick={check}>
            Check against full recalculation
          </button>
        </div>
        <div class="bar view-bar">
          <span class="view-label">View</span>
          <span class="view-state" role="status" title="table.viewState(): the sort, filters, search and grouping, whether set in the grid or from code">
            {shapeText()}
          </span>
          <span class="grow" />
          <span class="view-label">From code</span>
          <button title={'table.sort([{ column: "net" }])'} onClick={() => void shapeFromCode("losses")}>
            Biggest losses first
          </button>
          <button title={'table.filter("desk", "=Rates"); table.group("trader")'} onClick={() => void shapeFromCode("rates")}>
            Rates desk, by trader
          </button>
          <button title={'table.sort([]); table.filter(…, ""); table.search(""); table.group(null)'} onClick={() => void shapeFromCode("clear")}>
            Clear
          </button>
        </div>
        <div class="bar">
          <button class="stream" aria-pressed={streaming() ? "true" : "false"} onClick={() => setStreaming(!streaming())}>
            {streaming() ? "Pause prices" : "Stream prices"}
          </button>
          <label class="rate-control" title="How many trades' marks move each second. Slow enough to follow by default; drag right for thousands.">
            price changes a second{" "}
            <input type="range" min="0" max="5.3" step="0.01" value={rateLog()} onInput={(e) => setRateLog(Number(e.currentTarget.value))} />{" "}
            <output>{whole.format(rate())}</output>
          </label>
          <span class="sep" />
          <label title="A named value used by the net column. Changing it recalculates net in every row.">
            fee_bps{" "}
            <input
              type="number"
              value={fee()}
              min="0"
              max="100"
              step="0.5"
              onInput={(e) => {
                const value = Number(e.currentTarget.value);
                if (!Number.isFinite(value)) return;
                setFee(value);
                void table().setValue("fee_bps", value);
              }}
            />
          </label>
          <span class="grow" />
          <label title="Drag back to see the grid as it was after an earlier update: values, totals, order and filters.">
            Time{" "}
            <input
              type="range"
              min={history()[0]}
              max={history()[1]}
              value={lookingBack() ?? history()[1]}
              step="1"
              onInput={(e) => {
                const epoch = Number(e.currentTarget.value);
                lookBack(epoch >= history()[1] ? null : epoch);
              }}
            />{" "}
            <output>{lookingBack() === null ? "now" : `update ${whole.format(lookingBack()!)}`}</output>
          </label>
          <button disabled={lookingBack() === null} onClick={() => lookBack(null)}>
            Back to now
          </button>
        </div>
        <div class="bar formula">
          <span class="fx" aria-hidden="true">
            ƒ
          </span>
          <label>
            New column{" "}
            <input ref={formulaName} type="text" class="code formula-name" placeholder="name" spellcheck="false" aria-label="New column name" onKeyDown={(e) => e.key === "Enter" && addColumn()} />
          </label>
          <label>
            ={" "}
            <input ref={formula} type="text" class="code formula-src" placeholder="pnl / notional * 10000" spellcheck="false" aria-label="New column formula" onKeyDown={(e) => e.key === "Enter" && addColumn()} />
          </label>
          <label>
            total{" "}
            <select ref={formulaTotal} aria-label="New column total">
              <For each={["sum", "avg", "min", "max", "count", "none"]}>{(t) => <option value={t}>{t}</option>}</For>
            </select>
          </label>
          <button class="primary" onClick={addColumn}>
            Add column
          </button>
          <span class={`note ${note().kind}`} role="status">
            {note().text}
          </span>
        </div>
        <div class="progress" style={{ width: `${progress()}%` }} />
      </section>

      <main>
        <Show when={book()} keyed>
          {(b) => (
            <Grid
              class="grid"
              table={b.table}
              keyColumn={{ title: "trade", width: 164 }}
              label="Trades"
              theme={theme()}
              columns={looks}
              loadMore={lazy() ? loadMore : undefined}
              ref={onView}
              onStatus={onStatus}
              onFocus={onFocus}
              onError={(m) => say(m, "bad")}
              onNote={(m) => say(m, "good")}
              onGroup={(col) => {
                setGroupSignal(col);
                drawChart();
              }}
            />
          )}
        </Show>
        <aside>
          <div class="card chart">
            <div class="card-head">
              <h2>Totals by group</h2>
              <select
                aria-label="Column to chart"
                value={chartName()}
                onChange={(e) => {
                  setChartName(e.currentTarget.value);
                  flush();
                  drawChart();
                }}
              >
                <For each={totalled()} keyed={(c) => c.name}>{(c) => <option value={c().name}>{`${c().total} of ${c().name}`}</option>}</For>
              </select>
            </div>
            <div ref={chartBox} class="chart-box" />
          </div>
          <div class="card">
            <h2>{why() ? `Why · ${why()!.title}` : "Why this value"}</h2>
            <pre class={["why", { hint: !why() }]}>
              {why()?.text ?? WHY_HINT}
            </pre>
          </div>
        </aside>
      </main>

      <footer>
        <span>
          <b>{whole.format(footer().rows)}</b> rows
        </span>
        <span>
          <b>{whole.format(footer().filtered)}</b> shown
        </span>
        <Show when={footer().groups > 0}>
          <span>
            <b>{whole.format(footer().groups)}</b> groups
          </span>
        </Show>
        <span>
          <b>{whole.format(footer().selected)}</b> selected
        </span>
        <Show when={range()}>
          {(r) => (
            <span>
              <b>{whole.format(r().cells)}</b> cells{" "}
              <Show when={r().numbers}>
                <b>{two.format(r().sum)}</b> sum <b>{two.format(r().avg)}</b> avg <b>{two.format(r().min)}</b> min <b>{two.format(r().max)}</b> max
              </Show>
            </span>
          )}
        </Show>
        <Show when={lazy()}>
          <span>
            {more().loading ? "Loading more rows…" : more().done ? "Every row is loaded." : "More rows load as you scroll."}{" "}
            <Show when={!more().loading && !more().done}>
              <button class="more" onClick={() => view?.fetchMore()}>
                Load more
              </button>
            </Show>
          </span>
        </Show>
        <ul
          class="keys"
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
