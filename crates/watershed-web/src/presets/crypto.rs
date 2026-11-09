// A crypto book, priced live: each input is a coin's price in US dollars.
use watershed_core::prelude::*;

pub fn build(g: &mut Graph) -> Result<(), CalcError> {
    let btc = g.input("btc", 75_040.0)?;
    let eth = g.input("eth", 2_133.4)?;
    let sol = g.input("sol", 87.14)?;
    let avax = g.input("avax", 8.892)?;
    let link = g.input("link", 10.724)?;
    let ada = g.input("ada", 0.1805)?;
    let xrp = g.input("xrp", 1.1088)?;
    let doge = g.input("doge", 0.07028)?;
    let limit = g.input("risk_limit", 450_000.0)?;

    let majors = g.define("majors_desk", btc * 2.0 + eth * 40.0)?;
    let alts = g.define(
        "alts_desk",
        sol * 600.0 + avax * 3_000.0 + link * 2_500.0 + ada * 100_000.0,
    )?;
    let payments = g.define("payments_desk", xrp * 30_000.0 + doge * 400_000.0)?;

    let book = g.define("book_value", majors + alts + payments)?;
    g.define("pnl", book - 400_000.0)?;
    let majors_share = g.define("majors_share", majors / book)?;
    let within_limit = g.define("within_limit", book.le(limit))?;
    let concentrated = g.define("concentrated", majors_share.gt(0.6))?;
    g.define(
        "status",
        when(within_limit)
            .then(when(concentrated).then("too concentrated").otherwise("ok"))
            .otherwise("over limit"),
    )?;
    Ok(())
}
