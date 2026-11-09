//! Each test here checks one promise the engine makes, named so the list of
//! tests reads as a specification. `cargo test` runs them all.

use watershed_core::prelude::*;
use watershed_core::{Ir, Value};

type Result = std::result::Result<(), CalcError>;

/// The names of the nodes the last update recalculated, in order.
fn recalculated(g: &Graph) -> Vec<&str> {
    g.last_recalculated().iter().map(|&id| g.name(id)).collect()
}

#[test]
fn a_new_node_has_its_value_straight_away() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 2.0)?;
    let b = g.input("b", 3.0)?;
    let sum = g.define("sum", a + b)?;
    assert_eq!(g.get(sum)?, 5.0);
    Ok(())
}

#[test]
fn only_nodes_that_depend_on_a_change_are_recalculated() -> Result {
    let mut g = Graph::new();
    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let fx = g.input("fx", 1.1)?;
    let subtotal = g.define("subtotal", qty * price)?;
    let total = g.define("total", subtotal * 1.2)?;
    let unrelated = g.define("unrelated", fx * 100.0)?;

    g.set(price, 3.0)?;
    let report = g.stabilize();

    assert_eq!(recalculated(&g), ["subtotal", "total"]);
    assert_eq!(report.recalculated, 2);
    assert_eq!(g.get(total)?, 36.0);
    assert_eq!(g.get(unrelated)?, 110.00000000000001); // untouched
    Ok(())
}

#[test]
fn an_update_stops_where_a_value_comes_out_the_same() -> Result {
    let mut g = Graph::new();
    let temperature = g.input("temperature", 20.0)?;
    let is_hot = g.define("is_hot", temperature.gt(30.0))?;
    let fan_speed = g.define("fan_speed", when(is_hot).then(100.0).otherwise(0.0))?;
    let power = g.define("power", fan_speed * 1.5)?;

    // 20 -> 21: is_hot is recalculated but is still false, so nothing above it is.
    g.set(temperature, 21.0)?;
    let report = g.stabilize();
    assert_eq!(recalculated(&g), ["is_hot"]);
    assert_eq!(report.changed, 0);

    // 21 -> 35: now is_hot changes, so the update goes all the way up.
    g.set(temperature, 35.0)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["is_hot", "fan_speed", "power"]);
    assert_eq!(g.get(power)?, 150.0);
    Ok(())
}

#[test]
fn setting_an_input_to_the_value_it_already_has_does_nothing() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    g.define("double", a * 2.0)?;

    g.set(a, 1.0)?;
    let report = g.stabilize();
    assert_eq!(report.inputs_changed, 0);
    assert_eq!(report.recalculated, 0);
    Ok(())
}

#[test]
fn several_changes_in_one_update_recalculate_shared_nodes_once() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let b = g.input("b", 2.0)?;
    let sum = g.define("sum", a + b)?;
    let total = g.define("total", sum * 10.0)?;

    g.set(a, 5.0)?;
    g.set(b, 6.0)?;
    let report = g.stabilize();

    assert_eq!(report.inputs_changed, 2);
    assert_eq!(recalculated(&g), ["sum", "total"]); // each once, not twice
    assert_eq!(g.get(total)?, 110.0);
    Ok(())
}

#[test]
fn the_last_set_of_an_input_before_an_update_wins() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let double = g.define("double", a * 2.0)?;

    g.set(a, 50.0)?;
    g.set(a, 1.0)?; // back to where it was
    let report = g.stabilize();
    assert_eq!(report.inputs_changed, 0);
    assert_eq!(g.get(double)?, 2.0);
    Ok(())
}

#[test]
fn reads_return_the_last_update_until_the_next_one() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let double = g.define("double", a * 2.0)?;

    g.set(a, 10.0)?;
    assert_eq!(g.get(a)?, 1.0, "the input itself has not changed yet");
    assert_eq!(g.get(double)?, 2.0);

    g.stabilize();
    assert_eq!(g.get(a)?, 10.0);
    assert_eq!(g.get(double)?, 20.0);
    Ok(())
}

#[test]
fn a_node_reached_by_two_paths_is_recalculated_once_and_never_sees_half_an_update() -> Result {
    // x feeds `plus_one` and `times_two`, which both feed `combined`. A naive
    // engine might recalculate `combined` after only one side has updated
    // (a "glitch") and again after the other. Ordering by height prevents both.
    let mut g = Graph::new();
    let x = g.input("x", 1.0)?;
    let plus_one = g.define("plus_one", x + 1.0)?;
    let times_two = g.define("times_two", x * 2.0)?;
    let combined = g.define("combined", plus_one + times_two)?;

    g.set(x, 10.0)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["plus_one", "times_two", "combined"]);
    assert_eq!(g.get(combined)?, 31.0);
    Ok(())
}

#[test]
fn a_node_defined_between_updates_takes_part_in_the_next_one() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    g.set(a, 7.0)?; // recorded, not yet applied
    let double = g.define("double", a * 2.0)?;
    assert_eq!(
        g.get(double)?,
        2.0,
        "built from the values of the last update"
    );

    g.stabilize();
    assert_eq!(g.get(double)?, 14.0);
    Ok(())
}

#[test]
fn integer_errors_are_values_that_flow_up_and_clear_when_fixed() -> Result {
    let mut g = Graph::new();
    let total = g.input("total", 100_i64)?;
    let count = g.input("count", 4_i64)?;
    let average = g.define("average", total / count)?;
    let doubled = g.define("doubled", average * 2)?;
    assert_eq!(g.get(doubled)?, 50);

    g.set(count, 0)?;
    g.stabilize();
    let err = g.get(doubled).unwrap_err();
    assert_eq!(
        err,
        CalcError::Evaluation {
            node: "doubled".into(),
            message: "division by zero".into()
        }
    );

    g.set(count, 5)?;
    g.stabilize();
    assert_eq!(g.get(doubled)?, 40);
    Ok(())
}

#[test]
fn integer_overflow_is_an_error_not_a_wrong_number() -> Result {
    let mut g = Graph::new();
    let big = g.input("big", i64::MAX)?;
    let more = g.define("more", big + 1)?;
    assert!(matches!(g.get(more), Err(CalcError::Evaluation { .. })));
    Ok(())
}

#[test]
fn only_the_chosen_branch_of_a_conditional_is_evaluated() -> Result {
    let mut g = Graph::new();
    let count = g.input("count", 0_i64)?;
    // Without lazy branches, 100 / 0 would make this an error.
    let safe = g.define("safe", when(count.equals(0)).then(0).otherwise(100 / count))?;
    assert_eq!(g.get(safe)?, 0);
    Ok(())
}

#[test]
fn booleans_strings_and_comparisons_work() -> Result {
    let mut g = Graph::new();
    let region = g.input("region", "EU".to_string())?;
    let amount = g.input("amount", 250.0)?;
    let is_eu = g.define("is_eu", region.equals("EU"))?;
    let is_large = g.define("is_large", amount.ge(100.0))?;
    let needs_review = g.define("needs_review", is_eu & is_large)?;
    let label = g.define("label", when(needs_review).then("review").otherwise("ok"))?;
    assert_eq!(g.get(label)?, "review");

    g.set(region, "US".to_string())?;
    g.stabilize();
    assert_eq!(g.get(label)?, "ok");
    Ok(())
}

#[test]
fn names_must_be_unique() -> Result {
    let mut g = Graph::new();
    g.input("a", 1.0)?;
    assert_eq!(
        g.input("a", 2.0).unwrap_err(),
        CalcError::DuplicateName("a".into())
    );
    Ok(())
}

#[test]
fn a_node_from_another_graph_is_refused() -> Result {
    let mut first = Graph::new();
    let mut second = Graph::new();
    let a = first.input("a", 1.0)?;
    assert_eq!(
        second.define("double", a * 2.0).unwrap_err(),
        CalcError::ForeignNode
    );
    assert_eq!(second.get(a).unwrap_err(), CalcError::ForeignNode);
    Ok(())
}

#[test]
fn untyped_expressions_are_type_checked_before_they_are_accepted() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let s = g.input("s", "text".to_string())?;

    // a + s: a number plus a string.
    let bad = Ir::Binary(
        watershed_core::BinOp::Add,
        g.reference(a.id()).into(),
        g.reference(s.id()).into(),
    );
    assert!(matches!(g.define_ir("bad", bad), Err(CalcError::Type(_))));

    // a + 1: fine.
    let good = Ir::Binary(
        watershed_core::BinOp::Add,
        g.reference(a.id()).into(),
        Ir::Const(Value::F64(1.0)).into(),
    );
    let id = g.define_ir("good", good)?;
    assert!(g.value(id).same_as(&Value::F64(2.0)));
    Ok(())
}

#[test]
fn the_untyped_api_only_sets_inputs_and_only_to_their_type() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let double = g.define("double", a * 2.0)?;
    assert!(matches!(
        g.set_value(double.id(), Value::F64(1.0)),
        Err(CalcError::NotAnInput(_))
    ));
    assert!(matches!(
        g.set_value(a.id(), Value::I64(1)),
        Err(CalcError::TypeMismatch { .. })
    ));
    // An error is allowed, for example to say a feed has failed.
    g.set_value(a.id(), Value::Error("feed down".into()))?;
    g.stabilize();
    assert!(matches!(g.get(double), Err(CalcError::Evaluation { .. })));
    Ok(())
}

#[test]
fn explain_shows_why_a_node_has_its_value() -> Result {
    let mut g = Graph::new();
    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let tax = g.input("tax_rate", 0.2)?;
    let subtotal = g.define("subtotal", qty * price)?;
    let total = g.define("total", subtotal * (1.0 + tax))?;

    g.set(qty, 12.0)?;
    g.stabilize();

    let expected = "\
total = 36  ← subtotal * (1 + tax_rate)  [changed]
├── tax_rate = 0.2  (input)
└── subtotal = 30  ← qty * price  [changed]
    ├── qty = 12  (input)  [changed]
    └── price = 2.5  (input)
";
    assert_eq!(g.explain(total), expected);
    Ok(())
}

#[test]
fn explain_shows_a_shared_node_once() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 1.0)?;
    let y = g.define("y", x + 1.0)?;
    let z = g.define("z", y * y + y)?;
    let top = g.define("top", y + z)?;
    let text = g.explain(top);
    assert_eq!(text.matches("y = 2").count(), 2, "{text}");
    assert_eq!(text.matches("(see above)").count(), 1, "{text}");
    let _ = z;
    Ok(())
}

#[test]
fn describe_lists_every_node_level_by_level() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let b = g.input("b", 2.0)?;
    let sum = g.define("sum", a + b)?;
    g.define("double", sum * 2.0)?;
    g.set(a, 5.0)?;
    g.stabilize();

    let expected = "\
4 nodes, after update 1   (* recalculated in the last update, ! value changed)
level 0
  #0 a      f64 = 5  input    !
  #1 b      f64 = 2  input
level 1
  #2 sum    f64 = 7  a + b    *!
level 2
  #3 double f64 = 14 sum * 2  *!
";
    assert_eq!(g.describe(), expected);
    Ok(())
}

#[test]
fn to_dot_draws_every_node_and_edge() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let sum = g.define("sum", a + 1.0)?;
    g.set(a, 2.0)?;
    g.stabilize();
    let dot = g.to_dot();
    assert!(dot.starts_with("digraph watershed {"));
    assert!(dot.contains("n0 [shape=box"));
    assert!(dot.contains("n1 [shape=ellipse"));
    assert!(dot.contains("n0 -> n1;"));
    let _ = sum;
    Ok(())
}

#[test]
fn every_nan_counts_as_the_same_value() -> Result {
    // Hardware may produce NaNs with different bits for the same calculation.
    // They must not count as a change, or updates would spread for no reason
    // (and incremental results could disagree with a full recalculation).
    let mut g = Graph::new();
    let x = g.input("x", f64::NAN)?;
    let y = g.define("y", x + 1.0)?;
    g.set(x, f64::from_bits(f64::NAN.to_bits() ^ 1))?; // a NaN with different bits
    let report = g.stabilize();
    assert_eq!(report.inputs_changed, 0);
    assert!(g.get(y)?.is_nan());

    // 0.0 and -0.0 are different values: 1/0 is +∞ but 1/-0 is -∞.
    let z = g.input("z", 0.0)?;
    let inverse = g.define("inverse", 1.0 / z)?;
    g.set(z, -0.0)?;
    g.stabilize();
    assert_eq!(g.get(inverse)?, f64::NEG_INFINITY);
    Ok(())
}

#[test]
fn dependents_lists_each_user_once_in_the_order_added() -> Result {
    // Nodes are added before and after updates, so some links have been
    // packed together and some have not.
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let b = g.define("b", a + 1.0)?;
    let c = g.define("c", a * 2.0)?;
    g.set(a, 2.0)?;
    g.stabilize();
    let d = g.define("d", a - 1.0)?;
    g.set(a, 3.0)?;
    g.stabilize();
    let e = g.define("e", b + a)?;
    g.set(a, 4.0)?;
    g.stabilize();

    assert_eq!(g.dependents(a.id()), [b.id(), c.id(), d.id(), e.id()]);
    assert_eq!(g.dependents(b.id()), [e.id()]);
    assert_eq!(g.get(e)?, 9.0);
    Ok(())
}

#[test]
fn a_conditional_only_waits_on_the_branch_it_took() -> Result {
    let mut g = Graph::new();
    let use_live = g.input("use_live", true)?;
    let live = g.input("live", 101.0)?;
    let end_of_day = g.input("end_of_day", 100.0)?;
    let price = g.define("price", when(use_live).then(live).otherwise(end_of_day))?;
    let value = g.define("value", price * 10.0)?;

    // The end-of-day price is not being used, so changing it recalculates nothing.
    g.set(end_of_day, 98.0)?;
    let report = g.stabilize();
    assert_eq!(report.recalculated, 0);
    assert_eq!(g.get(value)?, 1010.0);

    // Switching source reads the other branch from now on...
    g.set(use_live, false)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["price", "value"]);
    assert_eq!(g.get(value)?, 980.0);

    // ...so the live price is the one that no longer matters.
    g.set(live, 150.0)?;
    assert_eq!(g.stabilize().recalculated, 0);
    g.set(end_of_day, 99.0)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["price", "value"]);
    assert_eq!(g.get(value)?, 990.0);
    Ok(())
}

#[test]
fn choose_reads_only_the_option_it_picks() -> Result {
    let mut g = Graph::new();
    let which = g.input("which", 1_i64)?;
    let a = g.input("a", 10.0)?;
    let b = g.input("b", 20.0)?;
    let c = g.input("c", 30.0)?;
    let picked = g.define("picked", choose(which, [a, b, c]))?;
    assert_eq!(g.get(picked)?, 20.0);
    assert_eq!(g.reads(picked.id()), [which.id(), b.id()]);

    g.set(a, 11.0)?;
    g.set(c, 31.0)?;
    assert_eq!(g.stabilize().recalculated, 0, "a and c are not picked");

    g.set(b, 21.0)?;
    g.stabilize();
    assert_eq!(g.get(picked)?, 21.0);

    g.set(which, 2)?;
    g.stabilize();
    assert_eq!(g.get(picked)?, 31.0);
    assert_eq!(g.reads(picked.id()), [which.id(), c.id()]);
    Ok(())
}

#[test]
fn choosing_out_of_range_is_an_error_until_the_index_is_fixed() -> Result {
    let mut g = Graph::new();
    let which = g.input("which", 0_i64)?;
    let a = g.input("a", 1.0)?;
    let picked = g.define("picked", choose(which, [a]))?;

    g.set(which, 3)?;
    g.stabilize();
    let message = match g.get(picked) {
        Err(CalcError::Evaluation { message, .. }) => message,
        other => panic!("expected an error, got {other:?}"),
    };
    assert_eq!(
        message,
        "choice 3 is out of range: there are 1 options, numbered from 0"
    );

    g.set(which, 0)?;
    g.stabilize();
    assert_eq!(g.get(picked)?, 1.0);
    Ok(())
}

#[test]
fn explain_follows_only_what_a_node_read() -> Result {
    let mut g = Graph::new();
    let use_live = g.input("use_live", true)?;
    let live = g.input("live", 101.0)?;
    let end_of_day = g.input("end_of_day", 100.0)?;
    let price = g.define("price", when(use_live).then(live).otherwise(end_of_day))?;
    let text = g.explain(price);
    assert!(text.contains("live = 101"), "{text}");
    assert!(!text.contains("end_of_day = 100"), "{text}");
    // The picture still shows the branch not taken, faint and dashed.
    assert!(g.to_dot().contains("[style=dashed, color=gray]"));
    let _ = end_of_day;
    Ok(())
}

// ---------------------------------------------------------------- collections

/// An orders table: qty × price per row, with revenue, count and largest line.
struct Orders {
    table: Collection,
    qty: Field<f64>,
    price: Field<f64>,
    line: Column<f64>,
    revenue: Node<f64>,
    count: Node<i64>,
    largest: Node<f64>,
}

fn orders(g: &mut Graph, rows: usize) -> std::result::Result<Orders, CalcError> {
    let mut table = Collection::new(g, "orders");
    let qty = table.field::<f64>("qty")?;
    let price = table.field::<f64>("price")?;
    let line = table.column(g, "line", qty * price)?;
    let revenue = table.sum(g, "revenue", line)?;
    let count = table.count(g, "count")?;
    let largest = table.max(g, "largest", line)?;
    for i in 0..rows {
        table.upsert(
            g,
            &format!("o{i}"),
            &[qty.is(1.0 + i as f64), price.is(2.0)],
        )?;
    }
    g.stabilize();
    Ok(Orders {
        table,
        qty,
        price,
        line,
        revenue,
        count,
        largest,
    })
}

#[test]
fn changing_one_row_recalculates_that_row_and_the_totals_it_feeds() -> Result {
    let mut g = Graph::new();
    let Orders {
        table: mut orders,
        qty,
        revenue,
        count,
        ..
    } = orders(&mut g, 1000)?;
    assert_eq!(g.get(revenue)?, 1001000.0);
    assert_eq!(g.get(count)?, 1000);

    orders.upsert(&mut g, "o500", &[qty.is(0.0)])?;
    g.stabilize();
    // The row's line, the revenue and the largest line: not the count, and
    // none of the other 999 rows.
    assert_eq!(recalculated(&g), ["orders[500].line", "revenue", "largest"]);
    assert_eq!(g.get(revenue)?, 1001000.0 - 1002.0);
    g.check_consistency().map_err(CalcError::Invalid)?;
    Ok(())
}

#[test]
fn removed_rows_leave_the_totals_and_their_nodes_are_reused() -> Result {
    let mut g = Graph::new();
    let Orders {
        table: mut orders,
        qty,
        price,
        line,
        revenue,
        count,
        largest,
    } = orders(&mut g, 3)?;
    // Lines: 2, 4, 6.
    assert_eq!(g.get(largest)?, 6.0);

    assert!(orders.remove(&mut g, "o2")?);
    g.stabilize();
    assert_eq!(
        (g.get(revenue)?, g.get(count)?, g.get(largest)?),
        (6.0, 2, 4.0)
    );
    assert!(!orders.contains("o2"));

    let nodes = g.len();
    orders.upsert(&mut g, "new", &[qty.is(10.0), price.is(1.0)])?;
    g.stabilize();
    assert_eq!(g.len(), nodes, "the removed row's nodes were reused");
    assert_eq!(orders.get(&g, "new", line)?, 10.0);
    assert_eq!(
        (g.get(revenue)?, g.get(count)?, g.get(largest)?),
        (16.0, 3, 10.0)
    );
    g.check_consistency().map_err(CalcError::Invalid)?;
    Ok(())
}

#[test]
fn a_column_can_use_other_columns_and_any_node_in_the_graph() -> Result {
    let mut g = Graph::new();
    let fx = g.input("fx", 2.0)?;
    let mut trades = Collection::new(&g, "trades");
    let notional = trades.field::<f64>("notional")?;
    let side = trades.field::<String>("side")?;
    let signed = trades.column(
        &mut g,
        "signed",
        when(side.equals("sell"))
            .then(-notional)
            .otherwise(notional),
    )?;
    let converted = trades.column(&mut g, "converted", signed * fx)?;
    let net = trades.sum(&mut g, "net", converted)?;
    trades.upsert(
        &mut g,
        "t1",
        &[notional.is(100.0), side.is("buy".to_string())],
    )?;
    trades.upsert(
        &mut g,
        "t2",
        &[notional.is(30.0), side.is("sell".to_string())],
    )?;
    g.stabilize();
    assert_eq!(g.get(net)?, 140.0);

    // The exchange rate is used by every row.
    g.set(fx, 3.0)?;
    g.stabilize();
    assert_eq!(g.get(net)?, 210.0);
    g.check_consistency().map_err(CalcError::Invalid)?;
    Ok(())
}

#[test]
fn totals_and_columns_added_after_rows_include_them() -> Result {
    let mut g = Graph::new();
    let mut c = Collection::new(&g, "c");
    let x = c.field::<i64>("x")?;
    c.upsert(&mut g, "a", &[x.is(3)])?;
    c.upsert(&mut g, "b", &[x.is(4)])?;
    g.stabilize();
    let doubled = c.column(&mut g, "doubled", x * 2)?;
    let total = c.sum(&mut g, "total", doubled)?;
    let smallest = c.min(&mut g, "smallest", x)?;
    g.stabilize();
    assert_eq!((g.get(total)?, g.get(smallest)?), (14, 3));
    Ok(())
}

#[test]
fn rows_are_checked() -> Result {
    let mut g = Graph::new();
    let mut c = Collection::new(&g, "c");
    let x = c.field::<f64>("x")?;
    let y = c.field::<f64>("y")?;
    assert!(
        matches!(
            c.upsert(&mut g, "a", &[x.is(1.0)]),
            Err(CalcError::Invalid(_))
        ),
        "a new row needs every field"
    );
    c.upsert(&mut g, "a", &[x.is(1.0), y.is(2.0)])?;
    assert!(
        matches!(c.field::<f64>("z"), Err(CalcError::Invalid(_))),
        "fields come before rows"
    );
    assert!(!c.remove(&mut g, "nope")?);
    let other = Graph::new();
    assert!(matches!(
        c.upsert(&mut Graph::new(), "a", &[x.is(1.0)]),
        Err(CalcError::ForeignNode)
    ));
    let _ = other;
    Ok(())
}

#[test]
fn totals_of_no_rows() -> Result {
    let mut g = Graph::new();
    let mut c = Collection::new(&g, "c");
    let x = c.field::<f64>("x")?;
    let (sum, count, min) = (
        c.sum(&mut g, "sum", x)?,
        c.count(&mut g, "count")?,
        c.min(&mut g, "min", x)?,
    );
    assert_eq!(
        (g.get(sum)?, g.get(count)?, g.get(min)?),
        (0.0, 0, f64::INFINITY)
    );
    Ok(())
}

#[test]
fn explain_lists_the_first_rows_of_a_big_total() -> Result {
    let mut g = Graph::new();
    let Orders { revenue, .. } = orders(&mut g, 50)?;
    let text = g.explain(revenue);
    assert!(
        text.starts_with("revenue = 2550  ← sum of 50 members"),
        "{text}"
    );
    assert!(text.contains("… and 90 more"), "{text}");
    Ok(())
}

#[test]
fn a_float_sum_keeps_the_sign_of_zero_like_adding_the_rows() -> Result {
    // In IEEE arithmetic, -0.0 + -0.0 + -0.0 is -0.0. The tree's empty room
    // must not turn it into +0.0.
    let mut g = Graph::new();
    let mut c = Collection::new(&g, "c");
    let x = c.field::<f64>("x")?;
    let sum = c.sum(&mut g, "sum", x)?;
    for key in ["a", "b", "c"] {
        c.upsert(&mut g, key, &[x.is(-0.0)])?;
    }
    g.stabilize();
    assert_eq!(g.get(sum)?.to_bits(), (-0.0_f64).to_bits());
    Ok(())
}

#[test]
fn a_nan_row_makes_min_and_max_nan_however_many_rows_were_removed() -> Result {
    let mut g = Graph::new();
    let mut c = Collection::new(&g, "c");
    let x = c.field::<f64>("x")?;
    let largest = c.max(&mut g, "largest", x)?;
    c.upsert(&mut g, "a", &[x.is(1.0)])?;
    c.upsert(&mut g, "b", &[x.is(f64::NAN)])?;
    g.stabilize();
    assert!(g.get(largest)?.is_nan());
    // With "a" gone, the tree has empty room next to the NaN; still NaN.
    c.remove(&mut g, "a")?;
    g.stabilize();
    assert!(g.get(largest)?.is_nan());
    c.upsert(&mut g, "b", &[x.is(2.0)])?;
    g.stabilize();
    assert_eq!(g.get(largest)?, 2.0);
    Ok(())
}

// ---------------------------------------------------------------- groups

#[test]
fn groups_total_their_rows_and_follow_rows_that_move() -> Result {
    let mut g = Graph::new();
    let mut sales = Collection::new(&g, "sales");
    let region = sales.field::<String>("region")?;
    let amount = sales.field::<f64>("amount")?;
    let by_region = sales.group_sum(&mut g, "by_region", region, amount)?;
    let counts = sales.group_count(&mut g, "count_by_region", region)?;
    sales.upsert(&mut g, "s1", &[region.is("EU".into()), amount.is(10.0)])?;
    sales.upsert(&mut g, "s2", &[region.is("EU".into()), amount.is(5.0)])?;
    sales.upsert(&mut g, "s3", &[region.is("US".into()), amount.is(7.0)])?;
    g.stabilize();
    let eu = sales.group(&g, by_region, "EU".to_string()).unwrap();
    let us = sales.group(&g, by_region, "US".to_string()).unwrap();
    assert_eq!((g.get(eu)?, g.get(us)?), (15.0, 7.0));
    assert_eq!(sales.group_keys(by_region), ["EU", "US"]);

    // s2 moves to the US: both groups update by that one row.
    sales.upsert(&mut g, "s2", &[region.is("US".into())])?;
    g.stabilize();
    assert_eq!((g.get(eu)?, g.get(us)?), (10.0, 12.0));
    let us_count = sales.group(&g, counts, "US".to_string()).unwrap();
    assert_eq!(g.get(us_count)?, 2);

    // A new group appears with its first row.
    sales.upsert(&mut g, "s4", &[region.is("APAC".into()), amount.is(1.0)])?;
    g.stabilize();
    let apac = sales.group(&g, by_region, "APAC".to_string()).unwrap();
    assert_eq!(g.get(apac)?, 1.0);
    assert!(sales.group(&g, by_region, "LATAM".to_string()).is_none());

    // A group that empties keeps its node (at 0) but leaves the keys.
    sales.remove(&mut g, "s1")?;
    g.stabilize();
    assert_eq!(g.get(eu)?, 0.0);
    assert_eq!(sales.group_keys(by_region), ["APAC", "US"]);
    g.check_consistency().map_err(CalcError::Invalid)?;
    Ok(())
}

#[test]
fn changing_one_rows_group_recalculates_only_that_row_and_its_two_groups() -> Result {
    let mut g = Graph::new();
    let mut c = Collection::new(&g, "c");
    let key = c.field::<i64>("key")?;
    let x = c.field::<f64>("x")?;
    let totals = c.group_sum(&mut g, "t", key, x)?;
    for i in 0..1000 {
        c.upsert(&mut g, &format!("r{i}"), &[key.is(i % 10), x.is(1.0)])?;
    }
    g.stabilize();
    c.upsert(&mut g, "r5", &[key.is(7)])?;
    let report = g.stabilize();
    // r5's "in group 5" turns false and its new "in group 7" turns true; the
    // two group totals update. Nothing else in 1,000 rows and 10 groups.
    assert_eq!(recalculated(&g), ["c[5].t=5", "c[5].t=7", "t[5]", "t[7]"]);
    assert_eq!(report.recalculated, 4);
    let five = c.group(&g, totals, 5).unwrap();
    let seven = c.group(&g, totals, 7).unwrap();
    assert_eq!((g.get(five)?, g.get(seven)?), (99.0, 101.0));
    Ok(())
}

#[test]
fn a_grouping_added_after_rows_includes_them() -> Result {
    let mut g = Graph::new();
    let mut c = Collection::new(&g, "c");
    let team = c.field::<String>("team")?;
    let points = c.field::<i64>("points")?;
    c.upsert(&mut g, "a", &[team.is("red".into()), points.is(3)])?;
    c.upsert(&mut g, "b", &[team.is("blue".into()), points.is(4)])?;
    c.upsert(&mut g, "c", &[team.is("red".into()), points.is(5)])?;
    g.stabilize();
    let best = c.group_max(&mut g, "best", team, points)?;
    g.stabilize();
    let red = c.group(&g, best, "red".to_string()).unwrap();
    assert_eq!(g.get(red)?, 5);
    assert!(matches!(
        c.group_sum(&mut g, "best", team, points),
        Err(CalcError::DuplicateName(_))
    ));
    Ok(())
}

// ---------------------------------------------------------------- redefining

#[test]
fn a_redefined_node_keeps_its_old_value_until_the_next_update() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 2.0)?;
    let b = g.input("b", 10.0)?;
    let x = g.define("x", a * 2.0)?;
    let y = g.define("y", x + 1.0)?;
    g.redefine(x, b * 3.0)?;
    assert_eq!(g.get(x)?, 4.0); // not yet
    g.stabilize();
    assert_eq!(g.get(x)?, 30.0);
    assert_eq!(g.get(y)?, 31.0); // what uses it follows
    assert_eq!(recalculated(&g), ["x", "y"]);
    g.check_consistency().unwrap();

    // Now `a` is nothing to do with it.
    g.set(a, 5.0)?;
    g.stabilize();
    assert!(recalculated(&g).is_empty());
    g.set(b, 1.0)?;
    g.stabilize();
    assert_eq!(g.get(y)?, 4.0);
    Ok(())
}

#[test]
fn a_redefinition_that_gives_the_same_value_stops_there() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 2.0)?;
    let x = g.define("x", a * 2.0)?;
    g.define("y", x + 1.0)?;
    g.redefine(x, a + a)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["x"]);
    Ok(())
}

#[test]
fn a_node_can_be_redefined_to_use_nodes_made_after_it() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1_i64)?;
    let x = g.define("x", a + 1)?;
    let y = g.define("y", x * 10)?;
    // `later` sits above `x`, so `x` (and `y` above it) must move up.
    let later = g.define("later", a * 100)?;
    g.redefine(x, later + 1)?;
    g.stabilize();
    assert_eq!(g.get(y)?, 1010);
    let (x, later) = (g.find("x").unwrap(), g.find("later").unwrap());
    assert!(g.height(x) > g.height(later));
    g.set(a, 2)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["later", "x", "y"]);
    assert_eq!(g.get(y)?, 2010);
    g.check_consistency().unwrap();
    Ok(())
}

#[test]
fn a_node_cannot_be_redefined_to_use_itself() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let x = g.define("x", a + 1.0)?;
    let y = g.define("y", x * 2.0)?;
    let err = g.redefine(x, y + 1.0).unwrap_err();
    assert_eq!(
        err.to_string(),
        "\"x\" cannot use \"y\" → \"x\": that would make it depend on itself"
    );
    Ok(())
}

#[test]
fn redefining_cannot_change_a_type_or_touch_an_input() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    g.define("x", a + 1.0)?;
    // The typed API cannot even express a change of type; the untyped one
    // refuses it.
    let x = g.find("x").unwrap();
    let err = g.redefine_ir(x, Ir::Const(Value::Bool(true))).unwrap_err();
    assert!(matches!(err, CalcError::TypeMismatch { .. }), "{err}");
    let a = g.find("a").unwrap();
    let err = g.redefine_ir(a, Ir::Const(Value::F64(2.0))).unwrap_err();
    assert_eq!(
        err.to_string(),
        "\"a\" is an input, so it cannot be redefined"
    );
    Ok(())
}

// ---------------------------------------------------------------- text

#[test]
fn concat_joins_two_strings() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", "x".to_string())?;
    let joined = g.define("joined", a.concat("y"))?;
    assert_eq!(g.get(joined)?, "xy");
    g.set(a, "z".to_string())?;
    g.stabilize();
    assert_eq!(g.get(joined)?, "zy");
    Ok(())
}

#[test]
fn text_shows_each_type_of_value() -> Result {
    let mut g = Graph::new();
    let i = g.input("i", -3_i64)?;
    let f = g.input("f", 2.0)?;
    let b = g.input("b", true)?;
    let s = g.input("s", "plain".to_string())?;
    let (ti, tf) = (g.define("ti", text(i))?, g.define("tf", text(f))?);
    let (tb, ts) = (g.define("tb", text(b))?, g.define("ts", text(s))?);
    assert_eq!(g.get(ti)?, "-3");
    assert_eq!(g.get(tf)?, "2.0");
    assert_eq!(g.get(tb)?, "true");
    assert_eq!(g.get(ts)?, "plain");
    Ok(())
}

#[test]
fn text_names_the_numbers_that_are_not_numbers() -> Result {
    let mut g = Graph::new();
    let f = g.input("f", f64::INFINITY)?;
    let shown = g.define("shown", text(f))?;
    assert_eq!(g.get(shown)?, "Infinity");
    g.set(f, f64::NEG_INFINITY)?;
    g.stabilize();
    assert_eq!(g.get(shown)?, "-Infinity");
    g.set(f, f64::NAN)?;
    g.stabilize();
    assert_eq!(g.get(shown)?, "NaN");
    Ok(())
}

#[test]
fn an_error_passes_through_concat_and_text() -> Result {
    let mut g = Graph::new();
    let n = g.input("n", 0_i64)?;
    let bad = g.define("bad", 1_i64 / n)?;
    let shown = g.define("shown", text(bad))?;
    let joined = g.define("joined", shown.concat("!"))?;
    assert!(g.value(joined.id()).is_error());
    Ok(())
}

#[test]
fn a_join_that_comes_out_the_same_stops_there() -> Result {
    let mut g = Graph::new();
    let n = g.input("n", 1_i64)?;
    let shown = g.define("shown", text(n.abs()))?;
    let above = g.define("above", shown.concat("!"))?;
    g.set(n, -1)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["shown"]);
    assert_eq!(g.get(above)?, "1!");
    Ok(())
}

#[test]
fn text_that_grows_too_long_becomes_an_error() -> Result {
    let mut g = Graph::new();
    let start = g.input("start", "x".repeat(1024))?;
    let mut last = g.define("v0", start.concat(""))?;
    for i in 1..40 {
        last = g.define(&format!("v{i}"), last.concat(last))?;
    }
    let value = g.value(last.id()).to_string();
    assert!(
        value.contains("too long"),
        "{}",
        &value[..value.len().min(80)]
    );
    Ok(())
}

#[test]
fn concat_needs_strings_on_both_sides() {
    let mut g = Graph::new();
    let n = g.input("n", 1_i64).unwrap();
    let id = n.id();
    let bad = Ir::Binary(
        watershed_core::BinOp::Concat,
        std::sync::Arc::new(g.reference(id)),
        std::sync::Arc::new(g.reference(id)),
    );
    assert!(g.define_ir("bad", bad).is_err());
}

#[test]
fn explain_writes_a_chain_of_joins_without_brackets() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", "x".to_string())?;
    let joined = g.define("joined", a.concat("y").concat(text(1_i64)))?;
    let why = g.explain(joined);
    assert!(why.contains("a + \"y\" + text(1)"), "{why}");
    Ok(())
}

// ---------------------------------------------------------------- lists

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_list_holds_its_items_in_order_and_follows_them() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", "x".to_string())?;
    let l = g.define("l", list([a.into_expr(), "y".into_expr()]))?;
    assert_eq!(g.get(l)?, strings(&["x", "y"]));
    g.set(a, "z".to_string())?;
    g.stabilize();
    assert_eq!(g.get(l)?, strings(&["z", "y"]));
    Ok(())
}

#[test]
fn an_empty_list_is_a_list() -> Result {
    let mut g = Graph::new();
    let none: [Expr<String>; 0] = [];
    let l = g.define("l", list(none))?;
    assert!(g.get(l)?.is_empty());
    Ok(())
}

#[test]
fn concat_puts_one_list_after_another() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", strings(&["1", "2"]))?;
    let b = g.input("b", strings(&["3"]))?;
    let both = g.define("both", a.concat(b))?;
    assert_eq!(g.get(both)?, strings(&["1", "2", "3"]));
    Ok(())
}

#[test]
fn join_makes_one_string_from_a_list() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", strings(&["x", "y", "z"]))?;
    let line = g.define("line", a.join(":"))?;
    assert_eq!(g.get(line)?, "x:y:z");
    g.set(a, Vec::new())?;
    g.stabilize();
    assert_eq!(g.get(line)?, "");
    Ok(())
}

#[test]
fn text_shows_a_list_as_kotlin_does() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", strings(&["x", "y"]))?;
    let shown = g.define("shown", text(a))?;
    assert_eq!(g.get(shown)?, "[x, y]");
    Ok(())
}

#[test]
fn an_error_in_an_item_makes_the_whole_list_an_error() -> Result {
    let mut g = Graph::new();
    let n = g.input("n", 0_i64)?;
    let bad = g.define("bad", text(1_i64 / n))?;
    let l = g.define("l", list([bad.into_expr(), "ok".into_expr()]))?;
    assert!(g.value(l.id()).is_error());
    g.set(n, 1)?;
    g.stabilize();
    assert_eq!(g.get(l)?, strings(&["1", "ok"]));
    Ok(())
}

#[test]
fn a_list_that_comes_out_the_same_stops_there() -> Result {
    let mut g = Graph::new();
    let n = g.input("n", 1_i64)?;
    let l = g.define("l", list([text(n.abs())]))?;
    let line = g.define("line", l.join(","))?;
    g.set(n, -1)?;
    g.stabilize();
    assert_eq!(recalculated(&g), ["l"]);
    assert_eq!(g.get(line)?, "1");
    Ok(())
}

#[test]
fn lists_can_be_compared_for_equality() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", strings(&["x"]))?;
    let same = g.define("same", a.equals(strings(&["x"])))?;
    assert!(g.get(same)?);
    g.set(a, strings(&["y"]))?;
    g.stabilize();
    assert!(!g.get(same)?);
    Ok(())
}

#[test]
fn a_list_that_grows_too_long_becomes_an_error() -> Result {
    let mut g = Graph::new();
    let start = g.input("start", vec!["x".to_string(); 1024])?;
    let mut last = g.define("v0", start.concat(Vec::new()))?;
    for i in 1..40 {
        last = g.define(&format!("v{i}"), last.concat(last))?;
    }
    assert!(g.value(last.id()).to_string().contains("too long"));
    Ok(())
}

#[test]
fn joining_a_huge_list_of_huge_strings_is_an_error_not_a_crash() -> Result {
    let mut g = Graph::new();
    let big = "x".repeat(1 << 19);
    let start = g.input("start", vec![big; 8])?;
    let line = g.define("line", start.join(""))?;
    let shown = g.define("shown", text(start))?;
    assert!(g.value(line.id()).to_string().contains("too long"));
    assert!(g.value(shown.id()).to_string().contains("too long"));
    Ok(())
}

#[test]
fn a_list_item_must_be_a_string() {
    let mut g = Graph::new();
    let n = g.input("n", 1_i64).unwrap();
    let bad = Ir::List(vec![std::sync::Arc::new(g.reference(n.id()))]);
    assert!(g.define_ir("bad", bad).is_err());
}

#[test]
fn join_needs_a_list_and_a_string() {
    let mut g = Graph::new();
    let s = g.input("s", "x".to_string()).unwrap();
    let bad = Ir::Binary(
        watershed_core::BinOp::Join,
        std::sync::Arc::new(g.reference(s.id())),
        std::sync::Arc::new(g.reference(s.id())),
    );
    assert!(g.define_ir("bad", bad).is_err());
}

#[test]
fn explain_writes_lists_the_way_they_were_built() -> Result {
    let mut g = Graph::new();
    let a = g.input("a", "x".to_string())?;
    let l = g.define("l", list([a.into_expr(), "y".into_expr()]))?;
    let line = g.define("line", l.join(" "))?;
    assert!(
        g.explain(l).contains("listOf(a, \"y\")"),
        "{}",
        g.explain(l)
    );
    assert!(
        g.explain(line).contains("join(l, \" \")"),
        "{}",
        g.explain(line)
    );
    Ok(())
}
