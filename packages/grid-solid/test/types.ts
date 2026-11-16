// Checks of the signals' types, compiled by `npm run check` and never run.
// Each `@ts-expect-error` must be an error, or the check fails.

import type { Accessor } from "solid-js";
import { Table, type Group } from "@surefold/grid";
import { createGroups, createReading } from "../src/index.js";

export function signals() {
  const products = new Table({
    fields: { title: "text", price: "number", stock: "number" },
    formulas: { value: "price * stock" },
    totals: { stock: "sum", value: "sum" },
  });
  // createReading gives its function the table's own type.
  const value: Accessor<number | null> = createReading(products, (t) => t.total("value"));
  // @ts-expect-error: no column called "valeu"
  createReading(products, (t) => t.total("valeu"));
  // With isEqual, its arguments are the value's type.
  const keys: Accessor<string[]> = createReading(
    products,
    (t) => t.groups().map((g) => g.key),
    (a, b) => a.join() === b.join(),
  );
  const groups: Accessor<Group[]> = createGroups(products);
  return [value, keys, groups];
}
