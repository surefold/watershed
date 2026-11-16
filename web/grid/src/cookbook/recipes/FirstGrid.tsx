import { onCleanup } from "solid-js";
import { Table } from "@surefold/grid";
import { Grid, createTotal } from "@surefold/grid-solid";

const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

export function FirstGrid() {
  // The table holds the rows and works out the formulas and totals.
  const orders = new Table({
    fields: { item: "text", qty: "number", price: "number" },
    formulas: { line: "qty * price * (1 + tax)" },
    values: { tax: 0.2 }, // a named number formulas can use
    totals: { qty: "sum", line: "sum" },
  });
  orders.put("o-1", { item: "Kettle", qty: 2, price: 24 });
  orders.put("o-2", { item: "Teapot", qty: 1, price: 38 });
  orders.put("o-3", { item: "Mugs", qty: 6, price: 7.5 });
  onCleanup(() => orders.dispose());

  // A signal that follows the total, after every recalculation.
  const total = createTotal(orders, "line");

  return (
    <>
      <div class="bar">
        <button onClick={() => orders.set("o-1", "qty", orders.get("o-1")!.qty + 1)}>One more kettle</button>
        <button onClick={() => orders.setValue("tax", 0.25)}>Tax to 25%</button>
        <span class="grow" />
        <span>
          With tax: <b>{total() === null ? "–" : money.format(total()!)}</b>
        </span>
      </div>
      <Grid class="grid" table={orders} keyColumn={{ title: "order" }} columns={{ qty: { decimals: 0 } }} />
    </>
  );
}
