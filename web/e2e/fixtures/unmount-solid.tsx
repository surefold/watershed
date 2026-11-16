// The Solid cookbook's Effect recipe, and a button that unmounts it. Typechecked
// with web/grid, whose JSX it is.

import { Show, createSignal } from "solid-js";
import { render } from "solid-js/web";
import "@surefold/grid/grid.css";
import { EffectPrices } from "../../grid/src/cookbook/recipes/EffectPrices";

function Fixture() {
  const [mounted, setMounted] = createSignal(true);
  return (
    <>
      <button onClick={() => setMounted(false)}>Unmount</button>
      <Show when={mounted()} fallback={<p>Unmounted</p>}>
        <section id="recipe">
          <EffectPrices />
        </section>
      </Show>
    </>
  );
}

render(() => <Fixture />, document.getElementById("fixture")!);
