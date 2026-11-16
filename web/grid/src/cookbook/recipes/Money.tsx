import { onCleanup } from "solid-js";
import { Table } from "@surefold/grid";
import { Grid, createReading } from "@surefold/grid-solid";

const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "EUR" });

export function Money() {
  // A café's bill. Money is a decimal: exact, and given and read back as text.
  const bill = new Table({
    fields: { item: "text", qty: "decimal", price: "decimal" },
    formulas: {
      net: "qty * price",
      vat: "round(net * 0.2d, 2)", // 20%, to the cent: a tie goes to the even cent
      gross: "net + vat",
      // The same amount as an ordinary number, to compare.
      float: "float(gross)",
    },
    totals: { net: "sum", vat: "sum", gross: "sum", float: "sum" },
  });
  bill.putMany(["L1", "L2", "L3", "L4", "L5", "L6"], {
    item: ["Toast", "Water", "Tea", "Croissant", "Cookie", "Bagel"],
    qty: ["4", "3", "1", "4", "5", "1"],
    price: ["1.10", "0.10", "1.90", "1.70", "0.70", "2.40"],
  });
  onCleanup(() => bill.dispose());

  // The total as text, exactly; and the ordinary numbers' total, as JavaScript writes it.
  const exact = createReading(bill, (t) => t.totalExact("gross"));
  const float = createReading(bill, (t) => t.total("float"));

  return (
    <>
      <div class="bar">
        <button onClick={() => bill.put(`L${bill.size + 1}`, { item: "Cookie", qty: "1", price: "0.70" })}>Add a cookie</button>
        <span class="grow" />
        <span>
          To pay: <b>{exact() === null ? "–" : money.format(exact() as `${number}`)}</b>
        </span>
        <span class="muted">As numbers: {String(float())}</span>
      </div>
      <Grid
        class="grid"
        table={bill}
        keyColumn={{ title: "line", width: 70 }}
        columns={{ qty: { decimals: 0 }, float: { decimals: 15, width: 190 } }}
      />
    </>
  );
}
