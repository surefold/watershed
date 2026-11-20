import { For, onCleanup, onSettled } from "solid-js";
import { GroupChart, Table, type GridView } from "@surefold/grid";
import { Grid, createGroups } from "@surefold/grid-solid";

const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });

export function Charts() {
  const stock = new Table({
    fields: { item: "text", category: "text", price: "number", units: "number" },
    formulas: { value: "price * units" },
    totals: { units: "sum", value: "sum" },
    history: 60, // keep recent recalculations, for the total's trend line
  });
  stock.put("S-1", { item: "Oak desk", category: "furniture", price: 340, units: 12 });
  stock.put("S-2", { item: "Pine shelf", category: "furniture", price: 85, units: 40 });
  stock.put("S-3", { item: "Desk lamp", category: "lighting", price: 42, units: 75 });
  stock.put("S-4", { item: "Floor lamp", category: "lighting", price: 120, units: 18 });
  stock.put("S-5", { item: "Rug", category: "textiles", price: 210, units: 9 });
  stock.put("S-6", { item: "Cushion", category: "textiles", price: 24, units: 120 });
  stock.group("category"); // groups() is empty until the rows are grouped
  onCleanup(() => stock.dispose());

  // 1. The package's own chart: draw it again after every recalculation.
  let box!: HTMLDivElement;
  let view: GridView | undefined;
  onSettled(() => {
    const chart = new GroupChart(box, { onPick: (key) => view?.revealGroup(key) });
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
  });

  // 2. Your own chart: a signal that follows groups(), set when a group's totals change. Hand it to any chart library.
  const groups = createGroups(stock);
  const most = () => Math.max(1, ...groups().map((g) => g.totals.value ?? 0));

  return (
    <>
      <div class="bar">
        <button onClick={() => stock.set("S-6", "units", stock.get("S-6")!.units + 100)}>Restock cushions</button>
        <button onClick={() => stock.patch("S-1", { price: 290 })}>Desk on sale</button>
      </div>
      <div class="charts">
        <div ref={box} />
        <div>
          <svg class="bars" viewBox={`0 0 300 ${groups().length * 34}`} role="img" aria-label="Stock value by category">
            <For each={groups()}>
              {(g, k) => (
                <g transform={`translate(0 ${k() * 34})`}>
                  <text y="12">
                    {g.key} · {money.format(g.totals.value ?? 0)}
                  </text>
                  <rect y="17" height="10" rx="3" width={(300 * (g.totals.value ?? 0)) / most()} />
                </g>
              )}
            </For>
          </svg>
        </div>
      </div>
      <Grid class="grid" table={stock} ref={(v) => (view = v)} columns={{ units: { decimals: 0 }, value: { decimals: 0 } }} />
    </>
  );
}
