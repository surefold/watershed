import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@surefold/grid/grid.css";
import "./page.css";
import { App } from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
