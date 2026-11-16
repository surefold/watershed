import { createSignal, onCleanup, Show } from "solid-js";
import { Table, type GridView } from "@surefold/grid";
import { Grid } from "@surefold/grid-solid";

interface Product {
  id: number;
  title: string;
  category: string;
  price: number;
  stock: number;
}

const PAGE = 20;
const page = (skip: number) => `https://dummyjson.com/products?limit=${PAGE}&skip=${skip}&select=title,category,price,stock`;

export function PagedApi() {
  const products = new Table({
    fields: { title: "text", category: "text", price: "number", stock: "number" },
    formulas: { value: "price * stock" },
    totals: { stock: "sum", value: "sum" },
  });
  onCleanup(() => products.dispose());
  const [said, setSaid] = createSignal("");
  const [failed, setFailed] = createSignal(false);
  let view: GridView | undefined;

  // Called by the grid as the person nears the last row. If it throws, the grid
  // says so through onStatus, and asks for the same page on the next scroll
  // near the end, or when fetchMore() is called (the Try again button).
  async function loadMore(): Promise<boolean> {
    // The next page starts after the rows already in. Keys make a page asked for twice harmless.
    const response = await fetch(page(products.size));
    if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
    const { products: list, total } = (await response.json()) as { products: Product[]; total: number };
    for (const p of list) {
      products.put(`P-${p.id}`, { title: p.title, category: p.category, price: p.price, stock: p.stock });
    }
    const loaded = products.size;
    setSaid(`${loaded} of ${total} products loaded`);
    return loaded < total; // false: every page is in, stop asking
  }

  return (
    <>
      <div class="bar">
        <span class="muted">{said() || "Loading the first page…"}</span>
        <Show when={failed()}>
          <button onClick={() => void view?.fetchMore()}>Try again</button>
        </Show>
      </div>
      <Grid
        class="grid"
        table={products}
        keyColumn={{ title: "id", width: 70 }}
        columns={{ title: { width: 240 }, stock: { decimals: 0 } }}
        loadMore={loadMore}
        onStatus={(s) => {
          if (s.loading !== undefined) setFailed(Boolean(s.failed));
          if (s.failed) setSaid(`That page did not load (${s.error}).`);
        }}
        // The grid asks for more as the person scrolls; ask for the first page once it is drawn.
        ref={(v) => {
          view = v;
          void v.fetchMore();
        }}
      />
    </>
  );
}
