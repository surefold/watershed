//! A short tour: build a small graph, change an input, and look at what the
//! engine did. Run it with:
//!
//!     cargo run --example tour
//!
//! It also writes `tour.dot`, a picture of the graph. Draw it with Graphviz
//! (`dot -Tsvg tour.dot -o tour.svg`) or paste it into an online DOT viewer.

use watershed_core::prelude::*;

fn main() -> Result<(), CalcError> {
    let mut g = Graph::new();

    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let tax = g.input("tax_rate", 0.2)?;
    let threshold = g.input("free_shipping_over", 50.0)?;

    let subtotal = g.define("subtotal", qty * price)?;
    let with_tax = g.define("with_tax", subtotal * (1.0 + tax))?;
    let free_shipping = g.define("free_shipping", with_tax.ge(threshold))?;
    let shipping = g.define("shipping", when(free_shipping).then(0.0).otherwise(4.99))?;
    let total = g.define("total", with_tax + shipping)?;

    println!("== The graph as built\n");
    println!("{}", g.describe());

    println!("== price: 2.5 -> 3.0\n");
    g.set(price, 3.0)?;
    let report = g.stabilize();
    println!("{report:?}\n");
    println!("{}", g.describe());
    println!(
        "shipping was recalculated? {}",
        g.last_recalculated().contains(&shipping.id())
    );
    println!("(free_shipping came out the same, so the update stopped there)\n");

    println!("== Why is total {}?\n", g.get(total)?);
    println!("{}", g.explain(total));

    std::fs::write("tour.dot", g.to_dot()).expect("write tour.dot");
    println!(
        "wrote tour.dot (orange = recalculated and changed, grey dashed = recalculated, same value)"
    );
    Ok(())
}
