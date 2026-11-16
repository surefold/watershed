import { useEffect, useRef } from "react";
import { GroupChart, Table, type GridView, type Group } from "@surefold/grid";
import { Grid, useReading, useTable } from "@surefold/grid-react";

const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });

export function Charts() {
  const stock = useTable(() => {
    const t = new Table({
      fields: { item: "text", category: "text", price: "number", units: "number" },
      formulas: { value: "price * units" },
      totals: { units: "sum", value: "sum" },
      history: 60, // keep recent recalculations, for the total's trend line
    });
    t.put("S-1", { item: "Oak desk", category: "furniture", price: 340, units: 12 });
    t.put("S-2", { item: "Pine shelf", category: "furniture", price: 85, units: 40 });
    t.put("S-3", { item: "Desk lamp", category: "lighting", price: 42, units: 75 });
    t.put("S-4", { item: "Floor lamp", category: "lighting", price: 120, units: 18 });
    t.put("S-5", { item: "Rug", category: "textiles", price: 210, units: 9 });
    t.put("S-6", { item: "Cushion", category: "textiles", price: 24, units: 120 });
    t.group("category"); // groups() is empty until the rows are grouped
    return t;
  });
  const view = useRef<GridView>(null);

  // 1. The package's own chart: draw it again after every recalculation.
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const chart = new GroupChart(box.current!, { onPick: (key) => void view.current?.revealGroup(key) });
    const draw = () =>
      chart.update({
        title: "value by category",
        groups: stock.groups().map((g) => ({ key: g.key, rows: g.rows, value: g.totals.value })),
        total: stock.total("value"),
        trend: stock.totalTrend("value"),
        format: (v) => money.format(v),
      });
    draw();
    return stock.watch(draw);
  }, [stock]);

  // 2. Your own chart: useReading renders again when what it reads changes. It
  // compares with ===, so read the groups as a string, then parse them. Hand
  // them to any chart library.
  const read = useReading(stock, (t) => JSON.stringify(t.groups()));
  const groups = JSON.parse(read) as Group[];
  const most = Math.max(1, ...groups.map((g) => g.totals.value ?? 0));

  return (
    <>
      <div className="bar">
        <button onClick={() => stock.set("S-6", "units", stock.get("S-6")!.units + 100)}>Restock cushions</button>
        <button onClick={() => stock.patch("S-1", { price: 290 })}>Desk on sale</button>
      </div>
      <div className="charts">
        <div ref={box} />
        <div>
          <svg className="bars" viewBox={`0 0 300 ${groups.length * 34}`} role="img" aria-label="Stock value by category">
            {groups.map((g, k) => (
              <g key={g.key} transform={`translate(0 ${k * 34})`}>
                <text y="12">
                  {g.key} · {money.format(g.totals.value ?? 0)}
                </text>
                <rect y="17" height="10" rx="3" width={(300 * (g.totals.value ?? 0)) / most} />
              </g>
            ))}
          </svg>
        </div>
      </div>
      <Grid ref={view} className="grid" table={stock} columns={{ units: { decimals: 0 }, value: { decimals: 0 } }} />
    </>
  );
}
