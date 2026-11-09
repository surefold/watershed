// A small trading book: prices feed positions, positions feed desks.
use watershed_core::prelude::*;

pub fn build(g: &mut Graph) -> Result<(), CalcError> {
    let gold = g.input("gold", 2400.0)?;
    let oil = g.input("oil", 78.0)?;
    let wheat = g.input("wheat", 6.2)?;
    let eur_usd = g.input("eur_usd", 1.08)?;
    let limit = g.input("risk_limit", 60000.0)?;

    let gold_long = g.define("gold_long", gold * 20.0)?;
    let gold_hedge = g.define("gold_hedge", gold * -8.0)?;
    let oil_long = g.define("oil_long", oil * 300.0)?;
    let wheat_short = g.define("wheat_short", wheat * -1500.0)?;
    let eur_bond = g.define("eur_bond", eur_usd * 9000.0)?;

    let metals = g.define("metals_desk", gold_long + gold_hedge)?;
    let energy = g.define("energy_desk", oil_long + wheat_short)?;
    let rates = g.define("rates_desk", eur_bond * 1.0)?;
    let book = g.define("book_value", metals + energy + rates)?;
    let exposure = g.define("exposure", metals.abs() + energy.abs() + rates.abs())?;
    g.define("within_limit", exposure.le(limit))?;
    g.define("book_in_eur", book / eur_usd)?;
    Ok(())
}
