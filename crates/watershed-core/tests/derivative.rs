//! What `Graph::gradient` promises, one test a promise. The derivatives are
//! checked against the model itself, nudged: a derivative that disagreed
//! with its own formulas would show here whatever the rule it came from.

use watershed_core::prelude::*;
use watershed_core::{AggregateOp, NodeId, Type, Value};

type Result = std::result::Result<(), CalcError>;

fn number(g: &Graph, id: NodeId) -> f64 {
    match g.value(id) {
        Value::F64(x) => *x,
        other => panic!("{other} is not a number"),
    }
}

/// Each derivative in `gradient` is what `of` does when its input is nudged
/// up and down, to a relative `within`.
fn assert_matches_nudging(
    g: &mut Graph,
    of: NodeId,
    inputs: &[Input<f64>],
    gradient: &[NodeId],
    within: f64,
) {
    g.stabilize();
    let nudge = 1e-6;
    for (&input, &slope) in inputs.iter().zip(gradient) {
        let said = number(g, slope);
        let at = g.get(input).unwrap();
        g.set(input, at + nudge).unwrap();
        g.stabilize();
        let up = number(g, of);
        g.set(input, at - nudge).unwrap();
        g.stabilize();
        let down = number(g, of);
        g.set(input, at).unwrap();
        g.stabilize();
        let nudged = (up - down) / (2.0 * nudge);
        assert!(
            (said - nudged).abs() <= within * nudged.abs().max(1.0),
            "{}: the derivative is {said}, and nudging gives {nudged}",
            g.name(slope)
        );
    }
}

#[test]
fn a_derivative_is_a_node_named_after_what_it_is_taken_by() -> Result {
    let mut g = Graph::new();
    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let tax = g.input("tax_rate", 0.2)?;
    let subtotal = g.define("subtotal", qty * price)?;
    let total = g.define("total", subtotal * (1.0 + tax))?;

    let slopes = g.gradient(total.id(), &[qty.id(), price.id(), tax.id()], "slope")?;
    g.stabilize();
    assert_eq!(g.name(slopes[1]), "slope[price]");
    assert_eq!(number(&g, slopes[0]), 2.5 * 1.2);
    assert_eq!(number(&g, slopes[1]), 10.0 * 1.2);
    assert_eq!(number(&g, slopes[2]), 25.0);
    Ok(())
}

#[test]
fn a_derivative_follows_its_inputs_as_any_node_does() -> Result {
    let mut g = Graph::new();
    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let total = g.define("total", qty * price)?;
    let slopes = g.gradient(total.id(), &[price.id()], "slope")?;
    g.set(qty, 4.0)?;
    g.stabilize();
    assert_eq!(number(&g, slopes[0]), 4.0);
    Ok(())
}

#[test]
fn every_function_has_its_derivative() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 1.3)?;
    let y = g.input("y", 0.7)?;
    let z = g.input("z", -0.4)?;
    let curved = g.define("curved", (x * y).exp() - (x + 2.0).ln() + (y + 1.0).sqrt())?;
    let turned = g.define("turned", (x * z).sin() * (y - z).cos())?;
    let powers = g.define(
        "powers",
        x.pow(y) + (y + 2.0).pow(3.0) + (x * 0.0 + 2.0).pow(z),
    )?;
    let corners = g.define(
        "corners",
        (x - y).abs() + (x * 2.0).min(y + z) - (z * z).max(x / (y + 3.0)),
    )?;
    let chosen = g.define(
        "chosen",
        when(turned.gt(0.0))
            .then(curved * z)
            .otherwise(-(powers / x)),
    )?;
    let all = g.define("all", curved + turned * powers - corners / (chosen + 10.0))?;

    let inputs = [x, y, z];
    let by: Vec<NodeId> = inputs.iter().map(|i| i.id()).collect();
    let slopes = g.gradient(all.id(), &by, "d")?;
    // At several places, on both sides of every corner and condition.
    for (a, b, c) in [
        (1.3, 0.7, -0.4),
        (0.4, 1.9, 0.8),
        (2.2, 0.3, 1.1),
        (0.9, 1.4, -1.7),
    ] {
        g.set(x, a)?;
        g.set(y, b)?;
        g.set(z, c)?;
        assert_matches_nudging(&mut g, all.id(), &inputs, &slopes, 1e-7);
    }
    Ok(())
}

#[test]
fn a_derivative_passes_through_a_total_to_its_members() -> Result {
    let mut g = Graph::new();
    let rate = g.input("rate", 1.1)?;
    let mut rows = Collection::new(&g, "rows");
    let amount = rows.field::<f64>("amount")?;
    let grown = rows.column(&mut g, "grown", amount * rate * rate)?;
    let sum = rows.sum(&mut g, "sum", grown)?;
    let largest = rows.max(&mut g, "largest", grown)?;
    let smallest = rows.min(&mut g, "smallest", grown)?;
    for (key, value) in [("a", 3.0), ("b", 8.0), ("c", 5.0)] {
        rows.upsert(&mut g, key, &[amount.is(value)])?;
    }
    g.stabilize();
    let all = g.define("all", sum * 2.0 + largest * smallest)?;
    let slopes = g.gradient(all.id(), &[rate.id()], "d")?;
    assert_matches_nudging(&mut g, all.id(), &[rate], &slopes, 1e-7);

    // A row that is taken out no longer counts.
    rows.remove(&mut g, "b")?;
    assert_matches_nudging(&mut g, all.id(), &[rate], &slopes, 1e-7);
    Ok(())
}

#[test]
fn a_node_used_many_times_adds_up_a_term_for_each_use() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 0.8)?;
    let present = g.input("present", true)?;
    let mut members = Vec::new();
    for k in 0..40 {
        let part = g.define(&format!("part{k}"), (x * (k as f64 + 1.0)).sin() * x)?;
        members.push((part.id(), present.id()));
    }
    let sum = g.aggregate("sum", AggregateOp::Sum, Type::F64, 0, &members)?;
    let slopes = g.gradient(sum, &[x.id()], "d")?;
    assert_matches_nudging(&mut g, sum, &[x], &slopes, 1e-7);
    Ok(())
}

#[test]
fn a_node_the_result_does_not_depend_on_has_a_derivative_of_zero() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 2.0)?;
    let other = g.input("other", 5.0)?;
    let double = g.define("double", x * 2.0)?;
    let slopes = g.gradient(double.id(), &[x.id(), other.id()], "d")?;
    g.stabilize();
    assert_eq!(number(&g, slopes[0]), 2.0);
    assert_eq!(number(&g, slopes[1]), 0.0);
    Ok(())
}

#[test]
fn a_derivative_by_a_calculated_node_takes_that_node_as_free() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 3.0)?;
    let square = g.define("square", x * x)?;
    let result = g.define("result", square * 5.0 + x)?;
    // By `square` alone: not followed down to `x`, and `x` held still.
    let slopes = g.gradient(result.id(), &[square.id()], "d")?;
    g.stabilize();
    assert_eq!(number(&g, slopes[0]), 5.0);
    Ok(())
}

#[test]
fn a_condition_has_the_derivative_of_the_branch_it_takes() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 3.0)?;
    let capped = g.define("capped", when(x.gt(2.0)).then(x * 10.0).otherwise(x * x))?;
    let slopes = g.gradient(capped.id(), &[x.id()], "d")?;
    g.stabilize();
    assert_eq!(number(&g, slopes[0]), 10.0);
    g.set(x, 1.5)?;
    g.stabilize();
    assert_eq!(number(&g, slopes[0]), 3.0);
    Ok(())
}

#[test]
fn a_whole_number_on_the_way_is_refused_and_nothing_is_added() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 3.0)?;
    let count = g.input("count", 4i64)?;
    let more = g.define("more", count + 1i64)?;
    let text = g.define("text", text(more))?;
    let nodes = g.len();
    // By a whole number: refused.
    assert!(g.gradient(x.id(), &[more.id()], "d").is_err());
    // Of text: refused.
    assert!(g.gradient(text.id(), &[x.id()], "d").is_err());
    assert_eq!(g.len(), nodes);
    Ok(())
}

#[test]
fn a_long_formula_of_several_nodes_is_differentiated_in_steps() -> Result {
    let mut g = Graph::new();
    let x = g.input("x", 1.3)?;
    let y = g.input("y", 0.7)?;
    let z = g.input("z", -0.4)?;
    // One node, with a part that occurs three times and corners in it.
    let d = x - y;
    let long = g.define(
        "long",
        (d.clone() * z + (x * y).sin())
            .abs()
            .max((d.clone() * d.clone() - z).abs())
            .min(
                when(z.gt(0.0))
                    .then(d * x.exp())
                    .otherwise(y.cos() / (x + 2.0)),
            )
            * (x * y * z + 3.0).sqrt(),
    )?;
    let inputs = [x, y, z];
    let by: Vec<NodeId> = inputs.iter().map(|i| i.id()).collect();
    let before = g.len();
    let slopes = g.gradient(long.id(), &by, "d")?;
    // The steps are nodes, the last of them the formula's own value.
    let steps: Vec<NodeId> = g
        .node_ids()
        .skip(before)
        .filter(|&id| g.name(id).starts_with("d~long~step"))
        .collect();
    assert!(steps.len() > 10, "{} steps", steps.len());
    for (a, b, c) in [(1.3, 0.7, -0.4), (0.4, 1.9, 0.8), (2.2, 0.3, 1.1)] {
        g.set(x, a)?;
        g.set(y, b)?;
        g.set(z, c)?;
        assert_matches_nudging(&mut g, long.id(), &inputs, &slopes, 1e-7);
        let last = *steps.last().expect("there are steps");
        assert_eq!(number(&g, last), g.get(long)?);
    }
    Ok(())
}
