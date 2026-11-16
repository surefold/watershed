import { createSignal, onCleanup, Show } from "solid-js";
import { Table } from "@surefold/grid";
import { Grid, createTotal } from "@surefold/grid-solid";

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
  const products = new Table({
    fields: { title: "text", category: "text", price: "number", stock: "number", discount: "number" },
    formulas: {
      sale: "price * (1 - discount / 100)",
      value: "sale * stock",
    },
    totals: { price: "avg", stock: "sum", value: "sum" },
  });
  const value = createTotal(products, "value");
  const [status, setStatus] = createSignal<"loading" | "ready" | "failed">("loading");
  const [error, setError] = createSignal("");

  let controller: AbortController | undefined;
  onCleanup(() => {
    controller?.abort(); // a late answer must not write to a freed table
    products.dispose();
  });

  async function load() {
    controller?.abort();
    controller = new AbortController();
    setStatus("loading");
    try {
      const response = await fetch(PRODUCTS, { signal: controller.signal });
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
  }
  void load();

  return (
    <>
      <div class="bar">
        <button disabled={status() === "loading"} onClick={load}>
          Fetch again
        </button>
        <Show when={status() === "loading"}>
          <span class="muted">Loading…</span>
        </Show>
        <Show when={status() === "failed"}>
          <span class="bad">Could not load the products: {error()}</span>
        </Show>
        <span class="grow" />
        <span>
          Stock value: <b>{value() === null ? "–" : money.format(value()!)}</b>
        </span>
      </div>
      <Grid
        class="grid"
        table={products}
        keyColumn={{ title: "id", width: 70 }}
        columns={{ title: { width: 220 }, stock: { decimals: 0 }, discount: { decimals: 1 } }}
      />
    </>
  );
}
