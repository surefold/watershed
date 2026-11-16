// The cookbook page in React: each recipe running, with its source beside it.
// The source shown is the file itself, so what you read is what runs. The
// words are shared with the Solid page.

import { StrictMode, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import "@surefold/grid/grid.css";
import "../../../grid/src/cookbook/cookbook.css";
import { install, recipes } from "../../../grid/src/cookbook/text";
import { highlight, type Language } from "../../../grid/src/cookbook/highlight";
import { FirstGrid } from "./recipes/FirstGrid";
import { FromAnApi } from "./recipes/FromAnApi";
import { PagedApi } from "./recipes/PagedApi";
import { EditAndSave } from "./recipes/EditAndSave";
import { WebSocketPrices } from "./recipes/WebSocketPrices";
import { ServerSentEvents } from "./recipes/ServerSentEvents";
import { Charts } from "./recipes/Charts";
import { InAWorker } from "./recipes/InAWorker";

const sources = import.meta.glob<string>("./recipes/*.tsx", { query: "?raw", import: "default", eager: true });

const live: Record<string, { file: string; Run: ComponentType }> = {
  "first-grid": { file: "FirstGrid.tsx", Run: FirstGrid },
  fetch: { file: "FromAnApi.tsx", Run: FromAnApi },
  paged: { file: "PagedApi.tsx", Run: PagedApi },
  save: { file: "EditAndSave.tsx", Run: EditAndSave },
  websocket: { file: "WebSocketPrices.tsx", Run: WebSocketPrices },
  sse: { file: "ServerSentEvents.tsx", Run: ServerSentEvents },
  charts: { file: "Charts.tsx", Run: Charts },
  worker: { file: "InAWorker.tsx", Run: InAWorker },
};

const REPO = "https://github.com/surefold/watershed/blob/main/web/grid-react/src/cookbook/recipes/";

/** A block of code, highlighted. */
function Code({ text, language }: { text: string; language: Language }) {
  return (
    <pre>
      <code dangerouslySetInnerHTML={{ __html: highlight(text, language) }} />
    </pre>
  );
}

function Source({ file }: { file: string }) {
  const text = sources[`./recipes/${file}`] ?? "";
  return (
    <>
      <div className="file">
        <a href={REPO + file}>{file}</a>
        <button onClick={() => void navigator.clipboard?.writeText(text)}>Copy</button>
      </div>
      <pre>
        <code dangerouslySetInnerHTML={{ __html: highlight(text, "tsx") }} />
      </pre>
    </>
  );
}

function Cookbook() {
  return (
    <>

      <nav className="toc" aria-label="Recipes">
        <a href="#setup">Setting up</a>
        {recipes.map((r) => (
          <a key={r.id} href={`#${r.id}`}>
            {r.title}
          </a>
        ))}
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
        <Code text={install.react} language="bash" />
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

      {recipes.map((r) => {
        const { file, Run } = live[r.id];
        return (
          <section key={r.id} id={r.id}>
            <h2>
              <a href={`#${r.id}`}>{r.title}</a>
            </h2>
            {r.intro.map((p, k) => (
              <p key={k} dangerouslySetInnerHTML={{ __html: p }} />
            ))}
            <div className="live">
              <Run />
            </div>
            <Source file={file} />
            <ul>
              {r.notes.map((n, k) => (
                <li key={k} dangerouslySetInnerHTML={{ __html: n }} />
              ))}
            </ul>
          </section>
        );
      })}

      <section>
        <h2>Further</h2>
        <p>
          Every method and option is in the READMEs: <a href="https://github.com/surefold/watershed/tree/main/packages/grid">@surefold/grid</a> (the table and the
          grid), <a href="https://github.com/surefold/watershed/tree/main/packages/grid-react">@surefold/grid-react</a>. <a href="../">The grid page</a> streams prices into a
          <code>WorkerTable</code>, and{" "}
          <a href="https://github.com/surefold/watershed/tree/main/web/grid-react/src/App.tsx">its source</a> shows streaming, looking back and adding columns as it
          runs.
        </p>
      </section>
    </>
  );
}

// The page's header is in its HTML, so it shows before this runs; the recipes go under it.
createRoot(document.getElementById("cookbook")!).render(
  <StrictMode>
    <Cookbook />
  </StrictMode>,
);
