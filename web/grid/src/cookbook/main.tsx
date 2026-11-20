// The cookbook page in Solid: each recipe running, with its source beside it.
// The source shown is the file itself, so what you read is what runs.

import { For } from "solid-js";
import { render, type JSX } from "@solidjs/web";
import "@surefold/grid/grid.css";
import "./cookbook.css";
import { install, recipes } from "./text";
import { highlight, type Language } from "./highlight";
import { FirstGrid } from "./recipes/FirstGrid";
import { FromAnApi } from "./recipes/FromAnApi";
import { PagedApi } from "./recipes/PagedApi";
import { EditAndSave } from "./recipes/EditAndSave";
import { Money } from "./recipes/Money";
import { WebSocketPrices } from "./recipes/WebSocketPrices";
import { ServerSentEvents } from "./recipes/ServerSentEvents";
import { EffectPrices } from "./recipes/EffectPrices";
import { Charts } from "./recipes/Charts";
import { InAWorker } from "./recipes/InAWorker";

const sources = import.meta.glob<string>("./recipes/*.tsx", { query: "?raw", import: "default", eager: true });

const live: Record<string, { file: string; run: () => JSX.Element }> = {
  "first-grid": { file: "FirstGrid.tsx", run: FirstGrid },
  fetch: { file: "FromAnApi.tsx", run: FromAnApi },
  paged: { file: "PagedApi.tsx", run: PagedApi },
  save: { file: "EditAndSave.tsx", run: EditAndSave },
  money: { file: "Money.tsx", run: Money },
  websocket: { file: "WebSocketPrices.tsx", run: WebSocketPrices },
  sse: { file: "ServerSentEvents.tsx", run: ServerSentEvents },
  effect: { file: "EffectPrices.tsx", run: EffectPrices },
  charts: { file: "Charts.tsx", run: Charts },
  worker: { file: "InAWorker.tsx", run: InAWorker },
};

const REPO = "https://github.com/surefold/watershed/blob/main/web/grid/src/cookbook/recipes/";

/** A block of code, highlighted. */
function Code(props: { text: string; language: Language }) {
  return (
    <pre>
      <code innerHTML={highlight(props.text, props.language)} />
    </pre>
  );
}

function Source(props: { file: string }) {
  const text = sources[`./recipes/${props.file}`] ?? "";
  return (
    <>
      <div class="file">
        <a href={REPO + props.file}>{props.file}</a>
        <button onClick={() => void navigator.clipboard?.writeText(text)}>Copy</button>
      </div>
      <pre>
        <code innerHTML={highlight(text, "tsx")} />
      </pre>
    </>
  );
}

function Cookbook() {
  return (
    <>

      <nav class="toc" aria-label="Recipes">
        <a href="#setup">Setting up</a>
        <For each={recipes}>{(r) => <a href={`#${r.id}`}>{r.title}</a>}</For>
      </nav>

      <section id="setup">
        <h2>
          <a href="#setup">Setting up</a>
        </h2>
        <p>
          The packages are on GitHub Packages. Put this <code>.npmrc</code> beside your <code>package.json</code> (npm and Bun both read it), with a GitHub token that can read packages (a
          classic token with <code>read:packages</code>):
        </p>
        <Code text={install.npmrc} language="ini" />
        <Code text={install.solid} language="bash" />
        <p>
          Bun reads the same <code>.npmrc</code>. If your project keeps its settings in <code>bunfig.toml</code> instead:
        </p>
        <Code text={install.bunfig} language="toml" />
        <p>
          The engine is WebAssembly, loaded with a top-level <code>await</code>, so build for ES2022. With Vite:
        </p>
        <Code text={install.vite} language="typescript" />
        <Code text={install.css} language="typescript" />
      </section>

      <For each={recipes}>
        {(r) => (
          <section id={r.id}>
            <h2>
              <a href={`#${r.id}`}>{r.title}</a>
            </h2>
            <For each={r.intro}>{(p) => <p innerHTML={p} />}</For>
            <div class="live">{live[r.id].run()}</div>
            <Source file={live[r.id].file} />
            <ul>
              <For each={r.notes}>{(n) => <li innerHTML={n} />}</For>
            </ul>
          </section>
        )}
      </For>

      <section>
        <h2>Further</h2>
        <p>
          Every method and option is in the READMEs: <a href="https://github.com/surefold/watershed/tree/main/packages/grid">@surefold/grid</a> (the table and the
          grid), <a href="https://github.com/surefold/watershed/tree/main/packages/grid-solid">@surefold/grid-solid</a>. <a href="../">The grid page</a> streams prices into a
          <code>WorkerTable</code>, and <a href="https://github.com/surefold/watershed/tree/main/web/grid/src/App.tsx">its source</a> shows streaming, looking back and adding columns as it runs.
        </p>
      </section>
    </>
  );
}

// The page's header is in its HTML, so it shows before this runs; the recipes go under it.
render(() => <Cookbook />, document.getElementById("cookbook")!);
