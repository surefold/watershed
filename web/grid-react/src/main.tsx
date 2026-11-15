import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@surefold/grid/grid.css";
// The same page styles as the Solid page.
import "../../grid/src/page.css";
import { App } from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
