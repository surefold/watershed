import { createSignal, onCleanup } from "solid-js";
import { Table } from "@surefold/grid";
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

  // Called by the grid as the person nears the last row.
  async function loadMore(): Promise<boolean> {
    try {
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
    } catch (e) {
      setSaid(`That page did not load (${(e as Error).message}). Scroll to try again.`);
      return true; // ask again on the next scroll
    }
  }

  return (
    <>
      <div class="bar">
        <span class="muted">{said() || "Loading the first page…"}</span>
      </div>
      <Grid
        class="grid"
        table={products}
        keyColumn={{ title: "id", width: 70 }}
        columns={{ title: { width: 240 }, stock: { decimals: 0 } }}
        loadMore={loadMore}
        // The grid asks for more as the person scrolls; ask for the first page once it is drawn.
        ref={(view) => void view.fetchMore()}
      />
    </>
  );
}
