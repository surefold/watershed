// Checks of the hooks' types, compiled by `npm run check` and never run.
// Each `@ts-expect-error` must be an error, or the check fails.

import { Table } from "@surefold/grid";
import { useGroups, useReading, useTable, useTotal } from "../src/index.js";

export function InlineTable() {
  // A table made inline keeps its field and formula names and kinds.
  const products = useTable(
    () =>
      new Table({
        fields: { title: "text", price: "number", stock: "number" },
        formulas: { value: "price * stock" },
        totals: { stock: "sum", value: "sum" },
      }),
  );
  products.put("1", { title: "Lamp", price: 42, stock: 3 });
  products.set("1", "price", 40);
  products.total("value");
  // @ts-expect-error: no field called "prcie"
  products.set("1", "prcie", 40);
  // @ts-expect-error: price is a number
  products.set("1", "price", "40");
  // @ts-expect-error: put needs every field
  products.put("2", { title: "Desk", price: 300 });
  // @ts-expect-error: no column called "valeu"
  products.total("valeu");

  // So does one made and filled inside the function.
  const stock = useTable(() => {
    const t = new Table({ fields: { item: "text", units: "number" }, totals: { units: "sum" } });
    t.put("S-1", { item: "Rug", units: 9 });
    return t;
  });
  // @ts-expect-error: no field called "unit"
  stock.patch("S-1", { unit: 3 });

  // useReading gives its function the table's own type.
  const value: number | null = useReading(products, (t) => t.total("value"));
  // @ts-expect-error: no column called "valeu"
  useReading(products, (t) => t.total("valeu"));
  // With isEqual, its arguments are the value's type.
  const titles: string[] = useReading(
    products,
    (t) => t.groups().map((g) => g.key),
    (a, b) => a.join() === b.join(),
  );
  const groups = useGroups(products);
  const units: number | null = useTotal(stock, "units");
  return [value, titles, groups[0]?.totals.value, units];
}
