// An online order: shipping is free above a threshold.
use watershed_core::prelude::*;

pub fn build(g: &mut Graph) -> Result<(), CalcError> {
    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let tax = g.input("tax_rate", 0.2)?;
    let threshold = g.input("free_shipping_over", 50.0)?;

    let subtotal = g.define("subtotal", qty * price)?;
    let with_tax = g.define("with_tax", subtotal * (1.0 + tax))?;
    let free_shipping = g.define("free_shipping", with_tax.ge(threshold))?;
    let shipping = g.define("shipping", when(free_shipping).then(0.0).otherwise(4.99))?;
    g.define("total", with_tax + shipping)?;
    Ok(())
}
