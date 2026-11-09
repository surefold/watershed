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
