import { createSignal, For, onCleanup, Show } from "solid-js";
import { Table } from "@surefold/grid";
import { Grid } from "@surefold/grid-solid";

// DummyJSON answers a PATCH as if it had saved it, but keeps nothing: fetch again and the old values are back.
const API = "https://dummyjson.com/products";
const FIELDS = ["title", "price", "stock"] as const;
type Product = { title: string; price: number; stock: number };

/** What changed in a row since it was last saved: only the fields that differ. */
function changes(now: Product, saved: Product): Partial<Product> {
  return Object.fromEntries(FIELDS.filter((f) => now[f] !== saved[f]).map((f) => [f, now[f]]));
}

export function EditAndSave() {
  const products = new Table({
    fields: { title: "text", price: "number", stock: "number" },
    formulas: { value: "price * stock" },
    totals: { stock: "sum", value: "sum" },
  });
  // The rows as the server last had them, by key (the product's id).
  const saved = new Map<string, Product>();
  const [unsaved, setUnsaved] = createSignal<[string, Partial<Product>][]>([]);
  const [saving, setSaving] = createSignal(false);
  const [log, setLog] = createSignal<string[]>([]);
  const say = (line: string) => setLog((l) => [line, ...l].slice(0, 6));

  // After each recalculation that changed an input (a person typing, pasting,
  // filling a block or undoing), compare every row with what the server has.
  // Undoing back to the saved value makes a row clean again.
  function compare() {
    const dirty: [string, Partial<Product>][] = [];
    for (const [key, before] of saved) {
      const row = products.get(key);
      if (!row) continue;
      const diff = changes(row, before);
      if (Object.keys(diff).length) dirty.push([key, diff]);
    }
    setUnsaved(dirty);
  }
  const stop = products.watch((change) => change.inputsChanged && compare());
  onCleanup(() => {
    stop();
    products.dispose();
  });

  async function load() {
    const response = await fetch(`${API}?limit=12&select=title,price,stock`);
    const { products: list } = (await response.json()) as { products: (Product & { id: number })[] };
    for (const { id, title, price, stock } of list) {
      saved.set(String(id), { title, price, stock }); // first, so the rows are clean when they arrive
      products.put(String(id), { title, price, stock });
    }
  }
  load().catch((e: Error) => say(`Could not load: ${e.message}`));

  // One PATCH a changed row, with only its changed fields. A row that saves is
  // clean from then on; one that fails stays unsaved, to try again.
  async function save() {
    setSaving(true);
    await Promise.all(
      unsaved().map(async ([key, diff]) => {
        try {
          const response = await fetch(`${API}/${key}`, {
            method: "PATCH",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(diff),
          });
          if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
          // What was sent is now the server's. (An edit made while saving stays unsaved.)
          saved.set(key, { ...saved.get(key)!, ...diff });
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
      for (const [key] of unsaved()) products.put(key, saved.get(key)!);
    });
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
            Unsaved: {unsaved().map(([key, diff]) => `#${key} ${Object.keys(diff).join(", ")}`).join(" · ")}
          </Show>
        </span>
      </div>
      <Grid class="grid" table={products} keyColumn={{ title: "id", width: 70 }} columns={{ title: { width: 260 }, stock: { decimals: 0 } }} />
      <Show when={log().length}>
        <pre class="log">
          <For each={log()}>{(line) => <div>{line}</div>}</For>
        </pre>
      </Show>
    </>
  );
}
