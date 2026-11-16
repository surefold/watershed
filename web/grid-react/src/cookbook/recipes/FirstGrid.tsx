import { Table } from "@surefold/grid";
import { Grid, useTable, useTotal } from "@surefold/grid-react";

const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

export function FirstGrid() {
  // The table holds the rows and works out the formulas and totals.
  // useTable makes it once for this component, and frees it when the component goes.
  const orders = useTable(() => {
    const t = new Table({
      fields: { item: "text", qty: "number", price: "number" },
      formulas: { line: "qty * price * (1 + tax)" },
      values: { tax: 0.2 }, // a named number formulas can use
      totals: { qty: "sum", line: "sum" },
    });
    t.put("o-1", { item: "Kettle", qty: 2, price: 24 });
    t.put("o-2", { item: "Teapot", qty: 1, price: 38 });
    t.put("o-3", { item: "Mugs", qty: 6, price: 7.5 });
    return t;
  });

  // Renders again after every recalculation that changes the total.
  const total = useTotal(orders, "line");

  return (
    <>
      <div className="bar">
        <button onClick={() => orders.set("o-1", "qty", orders.get("o-1")!.qty + 1)}>One more kettle</button>
        <button onClick={() => orders.setValue("tax", 0.25)}>Tax to 25%</button>
        <span className="grow" />
        <span>
          With tax: <b>{total === null ? "–" : money.format(total)}</b>
        </span>
      </div>
      <Grid className="grid" table={orders} keyColumn={{ title: "order" }} columns={{ qty: { decimals: 0 } }} />
    </>
  );
}
