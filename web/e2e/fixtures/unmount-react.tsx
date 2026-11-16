// The React cookbook's Effect recipe, and a button that unmounts it. Typechecked
// with web/grid-react, whose JSX it is.

import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import "@surefold/grid/grid.css";
import { EffectPrices } from "../../grid-react/src/cookbook/recipes/EffectPrices";

function Fixture() {
  const [mounted, setMounted] = useState(true);
  return (
    <>
      <button onClick={() => setMounted(false)}>Unmount</button>
      {mounted ? (
        <section id="recipe">
          <EffectPrices />
        </section>
      ) : (
        <p>Unmounted</p>
      )}
    </>
  );
}

createRoot(document.getElementById("fixture")!).render(
  <StrictMode>
    <Fixture />
  </StrictMode>,
);
