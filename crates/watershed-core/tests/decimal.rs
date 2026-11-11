//! Each test here checks one promise about decimal numbers. The random
//! counterpart is in `properties.rs`, where decimals are in every graph.

use watershed_core::prelude::*;
use watershed_core::{Decimal, Rounding};

type Result = std::result::Result<(), CalcError>;

fn d(digits: &str) -> Decimal {
    digits.parse().expect("a decimal")
}

#[test]
fn decimals_add_up_exactly_where_floats_do_not() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", d("0.1"))?;
    let b = g.input("b", d("0.2"))?;
    let sum = g.define("sum", a + b)?;
    assert_eq!(g.get(sum)?, d("0.3"));
    assert_eq!(g.get(sum)?.to_string(), "0.3");

    let fa = g.input("fa", 0.1)?;
    let fb = g.input("fb", 0.2)?;
    let fsum = g.define("fsum", fa + fb)?;
    assert_ne!(g.get(fsum)?, 0.3);
    Ok(())
}

#[test]
fn a_decimal_keeps_the_places_it_was_written_with() -> Result {
    let mut g = Graph::new();
    let price = g.input("price", d("1.50"))?;
    let qty = g.input("qty", d("3"))?;
    let line = g.define("line", price * qty)?;
    let shown = g.define("shown", text(line))?;
    assert_eq!(g.get(line)?.to_string(), "4.50");
    assert_eq!(g.get(shown)?, "4.50");
    assert_eq!(
        g.explain(line).lines().next(),
        Some("line = 4.50  ← price * qty")
    );
    Ok(())
}

#[test]
fn an_equal_decimal_written_differently_is_a_change_to_what_shows_it() -> Result {
    let mut g = Graph::new();
    let price = g.input("price", d("1.5"))?;
    let shown = g.define("shown", text(price))?;
    let double = g.define("double", price * d("2"))?;
    let big = g.define("big", price.gt(d("1")))?;
    assert_eq!(g.get(shown)?, "1.5");

    // 1.50 equals 1.5, but it shows differently, so it is passed on. The
    // comparison comes out the same, and the update stops there.
    g.set(price, d("1.50"))?;
    let report = g.stabilize();
    assert_eq!(report.inputs_changed, 1);
    assert_eq!(g.get(shown)?, "1.50");
    assert_eq!(g.get(double)?.to_string(), "3.00");
    assert!(g.get(big)?);
    assert_eq!((report.recalculated, report.changed), (3, 2));

    // The same value written the same way is no change at all.
    g.set(price, d("1.50"))?;
    assert_eq!(g.stabilize().inputs_changed, 0);
    Ok(())
}

#[test]
fn dividing_rounds_at_the_last_digit_and_by_zero_is_an_error() -> Result {
    let mut g = Graph::new();
    let one = g.input("one", d("1"))?;
    let by = g.input("by", d("3"))?;
    let third = g.define("third", one / by)?;
    assert_eq!(g.get(third)?.to_string(), "0.3333333333333333333333333333");

    g.set(by, d("8"))?;
    g.stabilize();
    assert_eq!(g.get(third)?.to_string(), "0.125");

    g.set(by, d("0"))?;
    g.stabilize();
    assert_eq!(
        g.get(third).unwrap_err().to_string(),
        "\"third\" could not be calculated: division by zero"
    );
    g.set(by, d("4"))?;
    g.stabilize();
    assert_eq!(g.get(third)?, d("0.25"));
    Ok(())
}

#[test]
fn rounding_puts_a_value_on_the_places_asked_for() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", d("2.345"))?;
    let places = g.input("places", 2_i64)?;
    let even = g.define("even", x.round(places))?;
    let half_up = g.define("half_up", x.round_with(places, Rounding::HalfUp))?;
    let down = g.define("down", x.round_with(places, Rounding::Down))?;
    let up = g.define("up", x.round_with(places, Rounding::Up))?;
    let all = |g: &Graph| -> std::result::Result<[String; 4], CalcError> {
        Ok([
            g.get(even)?.to_string(),
            g.get(half_up)?.to_string(),
            g.get(down)?.to_string(),
            g.get(up)?.to_string(),
        ])
    };
    // A tie: 2.345 is exactly between 2.34 and 2.35.
    assert_eq!(all(&g)?, ["2.34", "2.35", "2.34", "2.35"]);

    g.set(x, d("2.355"))?;
    g.stabilize();
    assert_eq!(all(&g)?, ["2.36", "2.36", "2.35", "2.36"]);

    g.set(x, d("-2.341"))?;
    g.stabilize();
    assert_eq!(all(&g)?, ["-2.34", "-2.34", "-2.34", "-2.35"]);

    // Fewer places than asked for is left alone; none is whole units.
    g.set(x, d("2.5"))?;
    g.stabilize();
    assert_eq!(g.get(even)?.to_string(), "2.5");
    g.set(places, 0)?;
    g.stabilize();
    assert_eq!(all(&g)?, ["2", "3", "2", "3"]);

    g.set(places, -1)?;
    g.stabilize();
    assert_eq!(
        g.get(even).unwrap_err().to_string(),
        "\"even\" could not be calculated: a decimal is rounded to between 0 and 28 places, not -1"
    );
    assert_eq!(
        g.explain(half_up).lines().next(),
        Some(
            "half_up = error: a decimal is rounded to between 0 and 28 places, not -1  ← round_half_up(x, places)  [changed]"
        )
    );
    Ok(())
}

#[test]
fn a_result_too_large_to_hold_is_an_error_not_a_wrong_number() -> Result {
    let mut g = Graph::new();
    let big = g.input("big", Decimal::MAX)?;
    let by = g.input("by", d("1"))?;
    let more = g.define("more", big + by)?;
    let product = g.define("product", big * by)?;
    assert_eq!(
        g.get(more).unwrap_err().to_string(),
        "\"more\" could not be calculated: decimal overflow"
    );
    assert_eq!(g.get(product)?, Decimal::MAX);

    g.set(by, d("-1"))?;
    g.stabilize();
    assert_eq!(g.get(more)?, Decimal::MAX - d("1"));
    Ok(())
}

#[test]
fn more_places_than_fit_are_rounded_at_the_last_one() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", d("0.00000000000000011"))?;
    let b = g.input("b", d("0.00000000000000011"))?;
    let product = g.define("product", a * b)?;
    // Exactly 0.0000000000000000000000000000000121: 34 places, of which 28 fit.
    assert_eq!(
        g.get(product)?.to_string(),
        "0.0000000000000000000000000000"
    );
    g.set(a, d("1100000"))?;
    g.stabilize();
    assert_eq!(g.get(product)?.to_string(), "0.00000000012100000");
    Ok(())
}

#[test]
fn decimals_convert_on_purpose_and_never_by_accident() -> Result {
    let mut g = Graph::new();
    let count = g.input("count", 3_i64)?;
    let price = g.input("price", d("9.99"))?;
    let total = g.define("total", count.to_decimal() * price)?;
    let rough = g.define("rough", total.to_f64() * 2.0)?;
    assert_eq!(g.get(total)?.to_string(), "29.97");
    assert_eq!(g.get(rough)?, 59.94);

    // Through the untyped API, the type checker refuses a mix.
    let mixed = watershed_core::Ir::Binary(
        watershed_core::BinOp::Add,
        std::sync::Arc::new(g.reference(price.id())),
        std::sync::Arc::new(g.reference(count.id())),
    );
    assert_eq!(
        g.define_ir("mixed", mixed).unwrap_err().to_string(),
        "type error: Add needs both sides to have the same type, got decimal and i64"
    );
    g.check_consistency().map_err(CalcError::Invalid)?;
    Ok(())
}

#[test]
fn a_collection_totals_decimals_exactly() -> Result {
    let mut g = Graph::new();
    let rate = g.input("rate", d("0.20"))?;
    let mut orders = Collection::new(&g, "orders");
    let net = orders.field::<Decimal>("net")?;
    let tax = orders.column(&mut g, "tax", (net * rate).round(2_i64))?;
    let gross = orders.column(&mut g, "gross", net + tax)?;
    let total = orders.sum(&mut g, "total", gross)?;
    let smallest = orders.min(&mut g, "smallest", gross)?;
    let largest = orders.max(&mut g, "largest", gross)?;
    // A thousand rows of 0.10: a float total would not be exactly 120.
    for i in 0..1000 {
        orders.upsert(&mut g, &format!("o{i}"), &[net.is(d("0.10"))])?;
    }
    g.stabilize();
    assert_eq!(g.get(total)?.to_string(), "120.00");
    assert_eq!(g.get(smallest)?, d("0.12"));

    // One row changes: its two columns and the three totals.
    orders.upsert(&mut g, "o7", &[net.is(d("19.99"))])?;
    assert_eq!(g.stabilize().recalculated, 5);
    assert_eq!(orders.get(&g, "o7", tax)?.to_string(), "4.00");
    assert_eq!(g.get(total)?.to_string(), "143.87");
    assert_eq!(g.get(largest)?.to_string(), "23.99");

    orders.remove(&mut g, "o7")?;
    g.set(rate, d("0.25"))?;
    g.stabilize();
    // 0.10 at 25% is 0.025 of tax: a tie, which goes to the even 0.02.
    assert_eq!(g.get(total)?.to_string(), "119.88");
    g.check_consistency().map_err(CalcError::Invalid)?;
    Ok(())
}

#[test]
fn rows_can_be_grouped_by_a_decimal() -> Result {
    let mut g = Graph::new();
    let mut trades = Collection::new(&g, "trades");
    let price = trades.field::<Decimal>("price")?;
    let qty = trades.field::<Decimal>("qty")?;
    let by_price = trades.group_sum(&mut g, "qty_by_price", price, qty)?;
    trades.upsert(&mut g, "a", &[price.is(d("1.50")), qty.is(d("2"))])?;
    trades.upsert(&mut g, "b", &[price.is(d("1.50")), qty.is(d("3"))])?;
    trades.upsert(&mut g, "c", &[price.is(d("2.25")), qty.is(d("1"))])?;
    g.stabilize();
    let at = trades.group(&g, by_price, d("1.50")).expect("a group");
    assert_eq!(g.get(at)?, d("5"));
    assert_eq!(trades.group_keys(&g, by_price), ["1.50", "2.25"]);
    Ok(())
}

#[test]
fn a_decimal_reads_as_it_was_and_explains_itself_then() -> Result {
    let mut g = Graph::new();
    let price = g.input("price", d("1.10"))?;
    let qty = g.input("qty", d("3"))?;
    let line = g.define("line", price * qty)?;
    g.keep_history(10);
    g.set(price, d("1.20"))?;
    g.stabilize(); // update 1
    g.set(price, d("1.30"))?;
    g.stabilize(); // update 2
    assert_eq!(g.get_at(line, 1)?.to_string(), "3.60");
    assert_eq!(
        g.explain_at(line, 1)?.lines().next(),
        Some("line = 3.60  ← price * qty  [changed]")
    );
    assert_eq!(g.get(line)?.to_string(), "3.90");
    Ok(())
}
