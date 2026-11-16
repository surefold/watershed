import { useCallback, useRef, useState } from "react";
import { Table, type GridView } from "@surefold/grid";
import { Grid, useTable } from "@surefold/grid-react";

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
  const products = useTable(
    () =>
      new Table({
        fields: { title: "text", category: "text", price: "number", stock: "number" },
        formulas: { value: "price * stock" },
        totals: { stock: "sum", value: "sum" },
      }),
  );
  const [said, setSaid] = useState("");
  const [failed, setFailed] = useState(false);
  const view = useRef<GridView | null>(null);

  // Called by the grid as the person nears the last row. If it throws, the grid
  // says so through onStatus, and asks for the same page on the next scroll
  // near the end, or when fetchMore() is called (the Try again button).
  const loadMore = useCallback(async (): Promise<boolean> => {
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
  }, [products]);

  // The grid asks for more as the person scrolls; ask for the first page once it is drawn.
  const firstPage = useCallback((v: GridView | null) => {
    view.current = v;
    void v?.fetchMore();
  }, []);

  return (
    <>
      <div className="bar">
        <span className="muted">{said || "Loading the first page…"}</span>
        {failed && <button onClick={() => void view.current?.fetchMore()}>Try again</button>}
      </div>
      <Grid
        ref={firstPage}
        className="grid"
        table={products}
        keyColumn={{ title: "id", width: 70 }}
        columns={{ title: { width: 240 }, stock: { decimals: 0 } }}
        loadMore={loadMore}
        onStatus={(s) => {
          if (s.loading !== undefined) setFailed(Boolean(s.failed));
          if (s.failed) setSaid(`That page did not load (${s.error}).`);
        }}
      />
    </>
  );
}
