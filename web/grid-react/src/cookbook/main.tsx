// The cookbook page in React: each recipe running, with its source beside it.
// The source shown is the file itself, so what you read is what runs. The
// words are shared with the Solid page.

import { StrictMode, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import "@surefold/grid/grid.css";
import "../../../grid/src/cookbook/cookbook.css";
import { install, recipes } from "../../../grid/src/cookbook/text";
import { FirstGrid } from "./recipes/FirstGrid";
import { FromAnApi } from "./recipes/FromAnApi";
import { PagedApi } from "./recipes/PagedApi";
import { Charts } from "./recipes/Charts";
import { InAWorker } from "./recipes/InAWorker";

const sources = import.meta.glob<string>("./recipes/*.tsx", { query: "?raw", import: "default", eager: true });

const live: Record<string, { file: string; Run: ComponentType }> = {
  "first-grid": { file: "FirstGrid.tsx", Run: FirstGrid },
  fetch: { file: "FromAnApi.tsx", Run: FromAnApi },
  paged: { file: "PagedApi.tsx", Run: PagedApi },
  charts: { file: "Charts.tsx", Run: Charts },
  worker: { file: "InAWorker.tsx", Run: InAWorker },
};

const REPO = "https://github.com/surefold/watershed/blob/main/web/grid-react/src/cookbook/recipes/";

function Source({ file }: { file: string }) {
  const text = sources[`./recipes/${file}`] ?? "";
  return (
    <>
      <div className="file">
        <a href={REPO + file}>{file}</a>
        <button onClick={() => void navigator.clipboard?.writeText(text)}>Copy</button>
      </div>
      <pre>
        <code>{text}</code>
      </pre>
    </>
  );
}

function Cookbook() {
  return (
    <div className="page">
      <header>
        <h1>
          Watershed <span>Grid</span> cookbook
        </h1>
        <p>
          Small, whole examples of the grid in an app: a first table, rows from an API, pages loaded as you scroll, charts that follow the table, and a large table in a worker. Each one runs
          here, and the code under it is the file that runs. <a href="../">Back to the grid</a>.
        </p>
        <div className="switch" aria-label="Framework">
          <a href="../../cookbook/">Solid</a>
          <span>React</span>
        </div>
      </header>

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
        <pre>{install.npmrc}</pre>
        <pre>{install.react}</pre>
        <p>
          Bun reads the same <code>.npmrc</code>. If your project keeps its settings in <code>bunfig.toml</code> instead:
        </p>
        <pre>{install.bunfig}</pre>
        <p>
          The engine is WebAssembly, loaded with a top-level <code>await</code>, so build for ES2022. With Vite:
        </p>
        <pre>{install.vite}</pre>
        <pre>{install.css}</pre>
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
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Cookbook />
  </StrictMode>,
);
