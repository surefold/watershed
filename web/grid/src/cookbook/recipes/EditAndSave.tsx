import { createSignal, For, onCleanup, Show } from "solid-js";
import { Table, type Edits } from "@surefold/grid";
import { Grid } from "@surefold/grid-solid";

// DummyJSON answers a PATCH as if it had saved it, but keeps nothing: fetch again and the old values are back.
const API = "https://dummyjson.com/products";
type Product = { title: string; price: number; stock: number };
type Field = keyof Product;

export function EditAndSave() {
  const products = new Table({
    fields: { title: "text", price: "number", stock: "number" },
    formulas: { value: "price * stock" },
    totals: { stock: "sum", value: "sum" },
  });
  onCleanup(() => products.dispose());
  // The rows as the server last had them, and the fields people changed since, by key (the product's id).
  const saved = new Map<string, Product>();
  const changed = new Map<string, Partial<Product>>();
  const [unsaved, setUnsaved] = createSignal<[string, Partial<Product>][]>([]);
  const [saving, setSaving] = createSignal(false);
  const [log, setLog] = createSignal<string[]>([]);
  const say = (line: string) => setLog((l) => [line, ...l].slice(0, 6));
  const show = () => setUnsaved([...changed].map(([key, fields]) => [key, { ...fields }]));

  // The grid says what a person changed: typing, pasting, filling a block,
  // undoing. Changes made from code (loading, discarding) are not reported.
  function onEdit(edits: Edits) {
    for (const { key, column, after } of edits.cells) {
      const field = column as Field; // only fields can be edited, never formulas
      const fields = changed.get(key) ?? {};
      // Back to what the server has (undone, or typed back): nothing to save.
      if (after === saved.get(key)?.[field]) delete fields[field];
      else Object.assign(fields, { [field]: after });
      if (Object.keys(fields).length) changed.set(key, fields);
      else changed.delete(key);
    }
    show();
  }

  async function load() {
    const response = await fetch(`${API}?limit=12&select=title,price,stock`);
    const { products: list } = (await response.json()) as { products: (Product & { id: number })[] };
    for (const { id, title, price, stock } of list) {
      saved.set(String(id), { title, price, stock });
      products.put(String(id), { title, price, stock });
    }
  }
  load().catch((e: Error) => say(`Could not load: ${e.message}`));

  // One PATCH a changed row, with only its changed fields. A row that fails
  // stays unsaved, to try again; a field changed again while saving stays too.
  async function save() {
    setSaving(true);
    await Promise.all(
      [...changed].map(async ([key, fields]) => {
        const sent = { ...fields };
        try {
          const response = await fetch(`${API}/${key}`, {
            method: "PATCH",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(sent),
          });
          if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
          saved.set(key, { ...saved.get(key)!, ...sent });
          const now = changed.get(key) ?? {};
          for (const field of Object.keys(sent) as Field[]) if (now[field] === sent[field]) delete now[field];
          if (!Object.keys(now).length) changed.delete(key);
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
      for (const key of changed.keys()) products.put(key, saved.get(key)!);
    });
    changed.clear();
    show();
  }

  return (
    <>
      <div class="bar">
        <button disabled={!unsaved().length || saving()} onClick={save}>
          {saving() ? "Saving…" : `Save ${unsaved().length} change${unsaved().length === 1 ? "" : "s"}`}
        </button>
        <button disabled={!unsaved().length || saving()} onClick={discard}>
          Discard
        </button>
        <span class="muted">
          <Show when={unsaved().length} fallback="Double-click a title, price or stock (or press Enter on it) to edit. Paste and ⌘Z work too.">
            Unsaved: {unsaved().map(([key, fields]) => `#${key} ${Object.keys(fields).join(", ")}`).join(" · ")}
          </Show>
        </span>
      </div>
      <Grid
        class="grid"
        table={products}
        onEdit={onEdit}
        keyColumn={{ title: "id", width: 70 }}
        columns={{ title: { width: 260 }, stock: { decimals: 0 } }}
      />
      <Show when={log().length}>
        <pre class="log">
          <For each={log()}>{(line) => <div>{line}</div>}</For>
        </pre>
      </Show>
    </>
  );
}
