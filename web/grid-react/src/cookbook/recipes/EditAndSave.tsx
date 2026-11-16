import { useEffect, useRef, useState } from "react";
import { Table, type Edits } from "@surefold/grid";
import { Grid, useTable } from "@surefold/grid-react";

// DummyJSON answers a PATCH as if it had saved it, but keeps nothing: fetch again and the old values are back.
const API = "https://dummyjson.com/products";
type Product = { title: string; price: number; stock: number };
type Field = keyof Product;

export function EditAndSave() {
  const products = useTable(
    () =>
      new Table({
        fields: { title: "text", price: "number", stock: "number" },
        formulas: { value: "price * stock" },
        totals: { stock: "sum", value: "sum" },
      }),
  );
  // The rows as the server last had them, and the fields people changed since, by key (the product's id).
  const saved = useRef(new Map<string, Product>());
  const changed = useRef(new Map<string, Partial<Product>>());
  const [unsaved, setUnsaved] = useState<[string, Partial<Product>][]>([]);
  const [saving, setSaving] = useState(false);
  const [log, setLog] = useState<string[]>([]);
  const say = (line: string) => setLog((l) => [line, ...l].slice(0, 6));
  const show = () => setUnsaved([...changed.current].map(([key, fields]) => [key, { ...fields }]));

  // The grid says what a person changed: typing, pasting, filling a block,
  // undoing. Changes made from code (loading, discarding) are not reported.
  function onEdit(edits: Edits) {
    for (const { key, column, after } of edits.cells) {
      const field = column as Field; // only fields can be edited, never formulas
      const fields = changed.current.get(key) ?? {};
      // Back to what the server has (undone, or typed back): nothing to save.
      if (after === saved.current.get(key)?.[field]) delete fields[field];
      else Object.assign(fields, { [field]: after });
      if (Object.keys(fields).length) changed.current.set(key, fields);
      else changed.current.delete(key);
    }
    show();
  }

  useEffect(() => {
    const controller = new AbortController();
    fetch(`${API}?limit=12&select=title,price,stock`, { signal: controller.signal })
      .then((response) => response.json() as Promise<{ products: (Product & { id: number })[] }>)
      .then(({ products: list }) => {
        for (const { id, title, price, stock } of list) {
          saved.current.set(String(id), { title, price, stock });
          products.put(String(id), { title, price, stock });
        }
      })
      .catch((e: Error) => e.name !== "AbortError" && say(`Could not load: ${e.message}`));
    return () => controller.abort();
  }, [products]);

  // One PATCH a changed row, with only its changed fields. A row that fails
  // stays unsaved, to try again; a field changed again while saving stays too.
  async function save() {
    setSaving(true);
    await Promise.all(
      [...changed.current].map(async ([key, fields]) => {
        const sent = { ...fields };
        try {
          const response = await fetch(`${API}/${key}`, {
            method: "PATCH",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(sent),
          });
          if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
          saved.current.set(key, { ...saved.current.get(key)!, ...sent });
          const now = changed.current.get(key) ?? {};
          for (const field of Object.keys(sent) as Field[]) if (now[field] === sent[field]) delete now[field];
          if (!Object.keys(now).length) changed.current.delete(key);
          say(`PATCH /products/${key} ${JSON.stringify(sent)} → saved`);
        } catch (e) {
          say(`PATCH /products/${key} failed (${(e as Error).message}); still unsaved`);
        }
      }),
    );
    setSaving(false);
    show();
  }

  // Put the server's values back, as one batch. Changes from code are not
  // reported to onEdit, so forget the unsaved ones here.
  function discard() {
    products.batch(() => {
      for (const key of changed.current.keys()) products.put(key, saved.current.get(key)!);
    });
    changed.current.clear();
    show();
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
            ? `Unsaved: ${unsaved.map(([key, fields]) => `#${key} ${Object.keys(fields).join(", ")}`).join(" · ")}`
            : "Double-click a title, price or stock (or press Enter on it) to edit. Paste and ⌘Z work too."}
        </span>
      </div>
      <Grid
        className="grid"
        table={products}
        onEdit={onEdit}
        keyColumn={{ title: "id", width: 70 }}
        columns={{ title: { width: 260 }, stock: { decimals: 0 } }}
      />
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
