import { useCallback, useEffect, useRef, useState } from "react";
import { Table } from "@surefold/grid";
import { Grid, useTable, useTotal } from "@surefold/grid-react";

// What the API sends for each product (only the fields asked for with `select`).
interface Product {
  id: number;
  title: string;
  category: string;
  price: number;
  stock: number;
  discountPercentage: number;
}

const PRODUCTS = "https://dummyjson.com/products?limit=0&select=title,category,price,stock,discountPercentage";
const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });

export function FromAnApi() {
  const products = useTable(
    () =>
      new Table({
        fields: { title: "text", category: "text", price: "number", stock: "number", discount: "number" },
        formulas: {
          sale: "price * (1 - discount / 100)",
          value: "sale * stock",
        },
        totals: { price: "avg", stock: "sum", value: "sum" },
      }),
  );
  const value = useTotal(products, "value");
  const [status, setStatus] = useState<"loading" | "ready" | "failed">("loading");
  const [error, setError] = useState("");
  const controller = useRef<AbortController>(null);

  const load = useCallback(async () => {
    controller.current?.abort();
    const mine = (controller.current = new AbortController());
    setStatus("loading");
    try {
      const response = await fetch(PRODUCTS, { signal: mine.signal });
      if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
      const { products: list } = (await response.json()) as { products: Product[] };
      // Every row at once, a column at a time. A key that is already there is replaced.
      products.putMany(
        list.map((p) => `P-${p.id}`),
        {
          title: list.map((p) => p.title),
          category: list.map((p) => p.category),
          price: list.map((p) => p.price),
          stock: list.map((p) => p.stock),
          discount: list.map((p) => p.discountPercentage),
        },
      );
      setStatus("ready");
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      setError((e as Error).message);
      setStatus("failed");
    }
  }, [products]);

  useEffect(() => {
    void load();
    // A late answer must not write to a table that has been freed.
    return () => controller.current?.abort();
  }, [load]);

  return (
    <>
      <div className="bar">
        <button disabled={status === "loading"} onClick={load}>
          Fetch again
        </button>
        {status === "loading" && <span className="muted">Loading…</span>}
        {status === "failed" && <span className="bad">Could not load the products: {error}</span>}
        <span className="grow" />
        <span>
          Stock value: <b>{value === null ? "–" : money.format(value)}</b>
        </span>
      </div>
      <Grid
        className="grid"
        table={products}
        keyColumn={{ title: "id", width: 70 }}
        columns={{ title: { width: 220 }, stock: { decimals: 0 }, discount: { decimals: 1 } }}
      />
    </>
  );
}
