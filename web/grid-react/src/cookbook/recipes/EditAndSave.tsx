import { useCallback, useEffect, useRef, useState } from "react";
import { Table } from "@surefold/grid";
import { Grid, useTable } from "@surefold/grid-react";

// DummyJSON answers a PATCH as if it had saved it, but keeps nothing: fetch again and the old values are back.
const API = "https://dummyjson.com/products";
const FIELDS = ["title", "price", "stock"] as const;
type Product = { title: string; price: number; stock: number };

/** What changed in a row since it was last saved: only the fields that differ. */
function changes(now: Product, saved: Product): Partial<Product> {
  return Object.fromEntries(FIELDS.filter((f) => now[f] !== saved[f]).map((f) => [f, now[f]]));
}

// Made outside the component so TypeScript keeps the field names and kinds.
const makeProducts = () =>
  new Table({
    fields: { title: "text", price: "number", stock: "number" },
    formulas: { value: "price * stock" },
    totals: { stock: "sum", value: "sum" },
  });

export function EditAndSave() {
  const products = useTable(makeProducts);
  // The rows as the server last had them, by key (the product's id).
  const saved = useRef(new Map<string, Product>());
  const [unsaved, setUnsaved] = useState<[string, Partial<Product>][]>([]);
  const [saving, setSaving] = useState(false);
  const [log, setLog] = useState<string[]>([]);
  const say = (line: string) => setLog((l) => [line, ...l].slice(0, 6));

  // After each recalculation that changed an input (a person typing, pasting,
  // filling a block or undoing), compare every row with what the server has.
  // Undoing back to the saved value makes a row clean again.
  const compare = useCallback(() => {
    const dirty: [string, Partial<Product>][] = [];
    for (const [key, before] of saved.current) {
      const row = products.get(key);
      if (!row) continue;
      const diff = changes(row, before);
      if (Object.keys(diff).length) dirty.push([key, diff]);
    }
    setUnsaved(dirty);
  }, [products]);
  useEffect(() => products.watch((change) => void (change.inputsChanged && compare())), [products, compare]);

  useEffect(() => {
    const controller = new AbortController();
    fetch(`${API}?limit=12&select=title,price,stock`, { signal: controller.signal })
      .then((response) => response.json() as Promise<{ products: (Product & { id: number })[] }>)
      .then(({ products: list }) => {
        for (const { id, title, price, stock } of list) {
          saved.current.set(String(id), { title, price, stock }); // first, so the rows are clean when they arrive
          products.put(String(id), { title, price, stock });
        }
      })
      .catch((e: Error) => e.name !== "AbortError" && say(`Could not load: ${e.message}`));
    return () => controller.abort();
  }, [products]);

  // One PATCH a changed row, with only its changed fields. A row that saves is
  // clean from then on; one that fails stays unsaved, to try again.
  async function save() {
    setSaving(true);
    await Promise.all(
      unsaved.map(async ([key, diff]) => {
        try {
          const response = await fetch(`${API}/${key}`, {
            method: "PATCH",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(diff),
          });
          if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
          // What was sent is now the server's. (An edit made while saving stays unsaved.)
          saved.current.set(key, { ...saved.current.get(key)!, ...diff });
          say(`PATCH /products/${key} ${JSON.stringify(diff)} → saved`);
        } catch (e) {
          say(`PATCH /products/${key} failed (${(e as Error).message}); still unsaved`);
        }
      }),
    );
    setSaving(false);
    compare();
  }

  // Put the server's values back, as one batch: one recalculation, one step to undo.
  function discard() {
    products.batch(() => {
      for (const [key] of unsaved) products.put(key, saved.current.get(key)!);
    });
  }

  return (
    <>
      <div className="bar">
        <button disabled={!unsaved.length || saving} onClick={save}>
          {saving ? "Saving…" : `Save ${unsaved.length} change${unsaved.length === 1 ? "" : "s"}`}
        </button>
        <button disabled={!unsaved.length || saving} onClick={discard}>
          Discard
        </button>
        <span className="muted">
          {unsaved.length
            ? `Unsaved: ${unsaved.map(([key, diff]) => `#${key} ${Object.keys(diff).join(", ")}`).join(" · ")}`
            : "Double-click a title, price or stock (or press Enter on it) to edit. Paste and ⌘Z work too."}
        </span>
      </div>
      <Grid className="grid" table={products} keyColumn={{ title: "id", width: 70 }} columns={{ title: { width: 260 }, stock: { decimals: 0 } }} />
      {log.length > 0 && (
        <pre className="log">
          {log.map((line, k) => (
            <div key={k}>{line}</div>
          ))}
        </pre>
      )}
    </>
  );
}
