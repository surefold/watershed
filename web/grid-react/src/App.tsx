// A portfolio in React, built only from the public API: a Table made from a
// spec, rows put in from JavaScript, prices moved in batches, and totals that
// React components follow with hooks. Rows never pass through React state.

import { useEffect, useRef, useState } from "react";
import { Table, themes, type GridView } from "@surefold/grid";
import { Grid, useChange, useTable, useTotal } from "@surefold/grid-react";

const SECTORS = ["Energy", "Health", "Tech", "Finance", "Industry", "Retail"] as const;
const NAMES = ["Arbor", "Basalt", "Cinder", "Delta", "Ember", "Fjord", "Garnet", "Harbor", "Indigo", "Juniper", "Kestrel", "Lumen"];

function makePortfolio() {
  const t = new Table({
    fields: { name: "text", sector: "text", shares: "number", cost: "number", price: "number" },
    values: { fx: 1 },
    formulas: {
      value: "shares * price * fx",
      gain: "shares * (price - cost) * fx",
    },
    totals: { shares: "sum", cost: "avg", price: "avg", value: "sum", gain: "sum" },
    history: 60,
  });
  for (let n = 1; n <= 2000; n++) {
    const cost = Math.round((10 + Math.random() * 190) * 100) / 100;
    t.put(`P-${String(n).padStart(4, "0")}`, {
      name: `${NAMES[n % NAMES.length]} ${Math.ceil(n / NAMES.length)}`,
      sector: SECTORS[n % SECTORS.length],
      shares: 10 * (1 + Math.floor(Math.random() * 50)),
      cost,
      price: Math.round(cost * (0.9 + Math.random() * 0.2) * 100) / 100,
    });
  }
  return t;
}

type Portfolio = ReturnType<typeof makePortfolio>;

const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });
const whole = new Intl.NumberFormat("en-US");

/** Follows one total; re-renders only when it moves. */
function Figure({ table, column, label }: { table: Portfolio; column: "value" | "gain"; label: string }) {
  const total = useTotal(table, column);
  return (
    <div className="figure">
      <span>{label}</span>
      <strong className={total !== null && total < 0 ? "neg" : undefined}>{total === null ? "–" : money.format(total)}</strong>
    </div>
  );
}

/** What the last recalculation cost. */
function Cost({ table }: { table: Portfolio }) {
  const change = useChange(table);
  if (!change) return null;
  return (
    <p className="cost">
      Last update: <b>{whole.format(change.inputsChanged)}</b> inputs changed, <b>{whole.format(change.recalculated)}</b> of{" "}
      {whole.format(change.nodes)} nodes recalculated, in <b>{change.ms.toFixed(2)} ms</b>.
    </p>
  );
}

export function App() {
  const table = useTable(makePortfolio);
  const view = useRef<GridView>(null);
  const [live, setLive] = useState(true);
  const [theme, setTheme] = useState("");
  // The page takes the grid's theme too: its colours read the grid's tokens.
  useEffect(() => {
    if (theme) document.documentElement.dataset.wgTheme = theme;
    else delete document.documentElement.dataset.wgTheme;
  }, [theme]);
  const [fx, setFx] = useState(1);
  const [why, setWhy] = useState<{ title: string; text: string } | null>(null);
  const [form, setForm] = useState({ key: "", name: "", sector: "Tech", shares: "100", cost: "50" });
  const [note, setNote] = useState("");

  // Move a few prices a few times a second, as one batch each time.
  useEffect(() => {
    if (!live) return;
    const timer = setInterval(() => {
      // Read first, then write them all in one batch: one recalculation.
      const moves: [string, number][] = [];
      for (let k = 0; k < 20; k++) {
        const key = `P-${String(1 + Math.floor(Math.random() * 2000)).padStart(4, "0")}`;
        const row = table.get(key);
        if (row) moves.push([key, Math.max(1, Math.round(row.price * (0.995 + Math.random() * 0.01) * 100) / 100)]);
      }
      table.batch(() => {
        for (const [key, price] of moves) table.set(key, "price", price);
      });
    }, 250);
    return () => clearInterval(timer);
  }, [table, live]);

  function add(e: React.FormEvent) {
    e.preventDefault();
    const key = form.key.trim() || `P-${String(table.size + 1).padStart(4, "0")}`;
    const cost = Number(form.cost);
    try {
      table.put(key, { name: form.name || key, sector: form.sector, shares: Number(form.shares), cost, price: cost });
      setNote(`Put ${key}.`);
    } catch (error) {
      setNote((error as Error).message);
    }
  }

  return (
    <>
      <section className="figures">
        <Figure table={table} column="value" label="Value" />
        <Figure table={table} column="gain" label="Gain" />
        <div className="controls">
          <button onClick={() => setLive(!live)} aria-pressed={live}>
            {live ? "Pause prices" : "Move prices"}
          </button>
          <label>
            fx{" "}
            <input
              type="number"
              step="0.01"
              value={fx}
              onChange={(e) => {
                const value = Number(e.target.value);
                setFx(value);
                if (Number.isFinite(value)) table.setValue("fx", value);
              }}
            />
          </label>
          <label>
            Group by{" "}
            <select onChange={(e) => view.current?.setGroup(Number(e.target.value))} defaultValue="-1">
              <option value="-1">nothing</option>
              <option value="1">sector</option>
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
        </div>
      </section>
      <Cost table={table} />

      <main>
        <Grid
          ref={view}
          className="grid"
          table={table}
          theme={theme}
          label="Positions"
          keyColumn={{ title: "position", width: 110 }}
          columns={{
            shares: { decimals: 0, width: 96 },
            price: { trend: true, width: 130 },
            value: { decimals: 0, width: 130 },
            gain: { decimals: 0, signed: true, width: 120 },
          }}
          onFocus={() => setWhy(view.current?.explainFocus() ?? null)}
          onError={setNote}
          onNote={setNote}
        />
        <aside>
          <form onSubmit={add}>
            <h2>Put a position</h2>
            <input placeholder="key (P-2001)" value={form.key} onChange={(e) => setForm({ ...form, key: e.target.value })} />
            <input placeholder="name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
            <select value={form.sector} onChange={(e) => setForm({ ...form, sector: e.target.value })}>
              {SECTORS.map((s) => (
                <option key={s}>{s}</option>
              ))}
            </select>
            <input type="number" placeholder="shares" value={form.shares} onChange={(e) => setForm({ ...form, shares: e.target.value })} />
            <input type="number" step="0.01" placeholder="cost" value={form.cost} onChange={(e) => setForm({ ...form, cost: e.target.value })} />
            <button type="submit">Put</button>
            <p className="note">{note}</p>
          </form>
          <div className="why">
            <h2>{why ? `Why · ${why.title}` : "Why this value"}</h2>
            <pre>{why?.text ?? "Click a cell or a total to see what it was worked out from."}</pre>
          </div>
        </aside>
      </main>
    </>
  );
}
