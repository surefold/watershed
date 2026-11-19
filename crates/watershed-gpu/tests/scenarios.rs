//! Scenarios must give what the graph gives: for each scenario, set the
//! inputs on the graph, update it, and read the outputs.

use watershed_core::prelude::*;
use watershed_core::{AggregateOp, Collection, NodeId, Type, Value, choose, text, when};
use watershed_gpu::{Kept, PlanError, Scenarios};

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// Mostly ordinary numbers, sometimes one that is awkward.
    fn value(&mut self) -> f64 {
        const AWKWARD: [f64; 8] = [
            0.0,
            -0.0,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            f64::MAX,
            f64::MIN_POSITIVE,
            1e-320,
        ];
        match self.below(8) {
            0 => AWKWARD[self.below(AWKWARD.len())],
            _ => (self.next() % 2_000_001) as f64 / 1000.0 - 1000.0,
        }
    }
}

/// The same number, or both not a number.
fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn assert_same(what: &str, got: &[f64], want: &[f64]) {
    assert_eq!(got.len(), want.len(), "{what}");
    for (k, (a, b)) in got.iter().zip(want).enumerate() {
        assert!(same(*a, *b), "{what}, result {k}: {a:e} is not {b:e}");
    }
}

struct Random {
    g: Graph,
    inputs: Vec<Input<f64>>,
    nodes: Vec<Node<f64>>,
}

/// A random graph of `f64` arithmetic, comparisons and conditions: each node
/// combines earlier nodes, inputs and constants.
fn random_graph(rng: &mut Rng, inputs: usize, nodes: usize) -> Random {
    let mut g = Graph::new();
    let inputs: Vec<Input<f64>> = (0..inputs)
        .map(|i| g.input(&format!("in{i}"), rng.value()).unwrap())
        .collect();
    let mut made: Vec<Node<f64>> = inputs.iter().map(|i| i.node()).collect();
    let mut flags: Vec<Node<bool>> = Vec::new();
    for n in 0..nodes {
        let a = made[rng.below(made.len())];
        let b = made[rng.below(made.len())];
        let c = made[rng.below(made.len())];
        let k = rng.value();
        if n % 7 == 3 {
            // A yes or no, as a node of its own.
            let flag = match rng.below(4) {
                0 => a.lt(b),
                1 => a.ge(k) & b.not_equals(c),
                2 => !a.le(b) | c.equals(k),
                _ => a.gt(b).equals(c.gt(k)),
            };
            flags.push(g.define(&format!("flag{n}"), flag).unwrap());
        }
        let expr = match rng.below(15) {
            0 => a + b,
            1 => a - b,
            2 => a * b,
            3 => a / b,
            4 => a.min(b),
            5 => a.max(b),
            6 => -a + k,
            7 => (a * k).abs(),
            8 => a * k + b * c,
            9 => (a + b).min(c - k).max(a / (b * b + 1.0)),
            10 => when(a.gt(b)).then(c).otherwise(a * k),
            11 => when(a.le(k) & b.not_equals(c)).then(a + b).otherwise(b / c),
            12 => when(!a.ge(b) | c.equals(k)).then(a - b).otherwise(c),
            13 if !flags.is_empty() => {
                let flag = flags[rng.below(flags.len())];
                when(flag).then(a.max(b)).otherwise(k)
            }
            _ => when(a.lt(0.0))
                .then(when(b.lt(0.0)).then(a * b).otherwise(-a))
                .otherwise(c),
        };
        made.push(g.define(&format!("n{n}"), expr).unwrap());
    }
    g.stabilize();
    let nodes = made.split_off(inputs.len());
    Random { g, inputs, nodes }
}

/// What the graph gives for each scenario, one after another.
fn by_the_graph(
    g: &mut Graph,
    vary: &[Input<f64>],
    outputs: &[NodeId],
    inputs: &[f64],
) -> Vec<f64> {
    let mut results = Vec::new();
    for scenario in inputs.chunks(vary.len()) {
        for (&input, &x) in vary.iter().zip(scenario) {
            g.set(input, x).unwrap();
        }
        g.stabilize();
        for &id in outputs {
            match g.value(id) {
                Value::F64(x) => results.push(*x),
                // A whole number (a count) comes out of a plan as its f64.
                Value::I64(x) => results.push(*x as f64),
                other => panic!("{} is {other}", g.name(id)),
            }
        }
    }
    results
}

fn ids(inputs: &[Input<f64>]) -> Vec<NodeId> {
    inputs.iter().map(|i| i.id()).collect()
}

/// Forty random graphs. In each, eight inputs are varied, four shared and
/// two left alone; the outputs are some of its nodes and one input of each
/// sort. `run` must give what the graph gives for fifty scenarios, and
/// again after the shared inputs are given new values, twice, on the same
/// plan.
fn gives_what_the_graph_gives(run: impl Fn(&Scenarios, &[f64]) -> Vec<f64>) {
    gives_what_the_graph_gives_in(false, run);
}

/// As above, with the plan's nodes in clusters if `clusters`.
fn gives_what_the_graph_gives_in(clusters: bool, run: impl Fn(&Scenarios, &[f64]) -> Vec<f64>) {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut r = random_graph(&mut rng, 14, 300);
        let vary: Vec<Input<f64>> = r.inputs[..8].to_vec();
        let shared: Vec<Input<f64>> = r.inputs[8..12].to_vec();
        let mut outputs: Vec<NodeId> = (0..20)
            .map(|_| r.nodes[rng.below(r.nodes.len())].id())
            .collect();
        outputs.push(r.nodes.last().unwrap().id());
        outputs.extend([r.inputs[0].id(), r.inputs[9].id(), r.inputs[13].id()]);

        let make = match clusters {
            true => Scenarios::with_clusters,
            false => Scenarios::with_shared,
        };
        let mut plan = make(&r.g, &ids(&vary), &ids(&shared), &outputs).unwrap();
        for round in 0..3 {
            if round > 0 {
                let values: Vec<f64> = shared.iter().map(|_| rng.value()).collect();
                plan.set_shared(&values);
                for (&input, &x) in shared.iter().zip(&values) {
                    r.g.set(input, x).unwrap();
                }
            }
            let inputs: Vec<f64> = (0..50 * vary.len()).map(|_| rng.value()).collect();
            let got = run(&plan, &inputs);
            let want = by_the_graph(&mut r.g, &vary, &outputs, &inputs);
            assert_same(&format!("seed {seed}, round {round}"), &got, &want);
        }
    }
}

#[test]
fn the_cpu_gives_what_the_graph_gives() {
    gives_what_the_graph_gives(|plan, inputs| plan.run_cpu(inputs));
}

#[test]
fn only_what_the_outputs_need_and_the_inputs_reach_is_calculated() {
    let mut g = Graph::new();
    let a = g.input("a", 1.0).unwrap();
    let b = g.input("b", 2.0).unwrap();
    let fixed = g.define("fixed", b * 10.0).unwrap();
    let wanted = g.define("wanted", a + fixed).unwrap();
    let _unwanted = g.define("unwanted", a * 3.0).unwrap();
    g.stabilize();
    let plan = Scenarios::new(&g, &[a.id()], &[wanted.id(), fixed.id()]).unwrap();
    assert_eq!(plan.nodes(), 1);
    assert_eq!(plan.levels(), 1);
    assert_eq!(plan.run_cpu(&[1.0, 5.0]), [21.0, 20.0, 25.0, 20.0]);
}

#[test]
fn a_node_is_in_the_level_after_the_highest_it_reads() {
    let mut g = Graph::new();
    let a = g.input("a", 1.0).unwrap();
    let one = g.define("one", a + 1.0).unwrap();
    let two = g.define("two", one * 2.0).unwrap();
    let both = g.define("both", a + two).unwrap();
    g.stabilize();
    let plan = Scenarios::new(&g, &[a.id()], &[both.id()]).unwrap();
    assert_eq!((plan.nodes(), plan.levels()), (3, 3));
    assert_eq!(plan.run_cpu(&[1.0, 2.0]), [5.0, 8.0]);
}

#[test]
fn what_depends_only_on_shared_inputs_is_worked_out_once() {
    let mut g = Graph::new();
    let a = g.input("a", 1.0).unwrap();
    let data = g.input("data", 3.0).unwrap();
    let doubled = g.define("doubled", data * 2.0).unwrap();
    let big = g.define("big", doubled.gt(10.0)).unwrap();
    let out = g
        .define("out", when(big).then(a + doubled).otherwise(a - doubled))
        .unwrap();
    g.stabilize();
    let outputs = [out.id(), doubled.id(), data.id()];
    let mut plan = Scenarios::with_shared(&g, &[a.id()], &[data.id()], &outputs).unwrap();
    // Only `out` is worked out for each scenario.
    assert_eq!((plan.nodes(), plan.shared_inputs()), (1, 1));
    assert_eq!(plan.run_cpu(&[1.0, 2.0]), [-5.0, 6.0, 3.0, -4.0, 6.0, 3.0]);
    plan.set_shared(&[8.0]);
    assert_eq!(
        plan.run_cpu(&[1.0, 2.0]),
        [17.0, 16.0, 8.0, 18.0, 16.0, 8.0]
    );
}

#[test]
fn a_condition_takes_the_branch_the_graph_takes() {
    let mut g = Graph::new();
    let a = g.input("a", 1.0).unwrap();
    let b = g.input("b", 2.0).unwrap();
    let picked = g
        .define("picked", when(a.gt(0.0)).then(a).otherwise(b))
        .unwrap();
    g.stabilize();
    let plan = Scenarios::new(&g, &[a.id()], &[picked.id()]).unwrap();
    // Not a number is not above zero.
    let got = plan.run_cpu(&[3.0, -3.0, 0.0, f64::NAN]);
    assert_eq!(got, [3.0, 2.0, 2.0, 2.0]);
}

/// A graph that uses every maths function, its varied input, and the
/// scenarios to try it on.
fn with_maths() -> (Scenarios, Graph, Input<f64>, Node<f64>, Vec<f64>) {
    let mut g = Graph::new();
    let x = g.input("x", 1.0).unwrap();
    let rate = g.input("rate", 0.05).unwrap();
    let grown = g.define("grown", (1.0 + rate).pow(x)).unwrap();
    let curve = g
        .define(
            "curve",
            (x * rate).exp() - (x + 1.0).ln() + x.sqrt() + x.sin() * 0.25 + (x * rate).cos()
                - (x * -1.7).floor() * 0.01,
        )
        .unwrap();
    let both = g.define("both", grown / curve).unwrap();
    g.stabilize();
    let plan = Scenarios::new(&g, &[x.id()], &[both.id()]).unwrap();
    let inputs = (0..200).map(|k| k as f64 * 0.37).collect();
    (plan, g, x, both, inputs)
}

#[test]
fn maths_functions_give_what_the_graph_gives() {
    let (plan, mut g, x, both, inputs) = with_maths();
    let got = plan.run_cpu(&inputs);
    for (&value, &got) in inputs.iter().zip(&got) {
        g.set(x, value).unwrap();
        g.stabilize();
        assert_eq!(g.get(both).unwrap().to_bits(), got.to_bits(), "x = {value}");
    }
}

#[test]
fn derivative_nodes_give_what_the_graph_gives() {
    // A sum of long formulas with corners in them, and its derivative by
    // every input: totals, conditions and formulas taken in steps.
    let mut g = Graph::new();
    let present = g.input("present", true).unwrap();
    let inputs: Vec<Input<f64>> = (0..6)
        .map(|k| g.input(&format!("x{k}"), 0.3 * k as f64 - 0.7).unwrap())
        .collect();
    let mut parts = Vec::new();
    for i in 0..inputs.len() {
        for j in i + 1..inputs.len() {
            let (a, b) = (inputs[i], inputs[j]);
            let d = a - b;
            let part = (d.clone() * a.cos() + b.sin())
                .abs()
                .max((d.clone() * d - 0.2).abs())
                .min(a * b + 1.5);
            let part = g
                .define(&format!("part{i}_{j}"), part.clone() * part)
                .unwrap();
            parts.push((part.id(), present.id()));
        }
    }
    let sum = g
        .aggregate("sum", AggregateOp::Sum, Type::F64, 0, &parts)
        .unwrap();
    let by: Vec<_> = inputs.iter().map(|i| i.id()).collect();
    let slopes = g.gradient(sum, &by, "slope").unwrap();
    g.stabilize();

    let mut outputs = vec![sum];
    outputs.extend(&slopes);
    let plan = Scenarios::new(&g, &by, &outputs).unwrap();
    let scenarios: Vec<f64> = (0..50 * by.len())
        .map(|k| ((k * 37 % 101) as f64 - 50.0) / 40.0)
        .collect();
    let got = plan.run_cpu(&scenarios);
    for (scenario, got) in scenarios.chunks(by.len()).zip(got.chunks(outputs.len())) {
        for (&input, &value) in inputs.iter().zip(scenario) {
            g.set(input, value).unwrap();
        }
        g.stabilize();
        for (&output, &got) in outputs.iter().zip(got) {
            let Value::F64(want) = *g.value(output) else {
                panic!("{} is not a number", g.name(output));
            };
            assert_eq!(want.to_bits(), got.to_bits(), "{}", g.name(output));
        }
    }
}

#[test]
fn a_chain_needs_room_for_a_few_nodes_at_once_not_all_of_them() {
    // Each node reads only the one before, so its slot is free two levels on.
    let mut g = Graph::new();
    let x = g.input("x", 1.0).unwrap();
    let mut last = g.define("n0", x + 1.0).unwrap();
    for k in 1..1000 {
        last = g.define(&format!("n{k}"), last * 1.0001 + 1.0).unwrap();
    }
    g.stabilize();
    let plan = Scenarios::new(&g, &[x.id()], &[last.id()]).unwrap();
    assert_eq!((plan.nodes(), plan.levels()), (1000, 1000));
    assert!(
        plan.bytes_per_scenario() <= 4 * 8,
        "{}",
        plan.bytes_per_scenario()
    );
    // Two shapes: the first node's and the rest's.
    assert_eq!(plan.shapes(), 2);
    g.set(x, 3.0).unwrap();
    g.stabilize();
    assert_eq!(plan.run_cpu(&[3.0]), [g.get(last).unwrap()]);
}

#[test]
fn what_cannot_be_done_is_refused_by_name() {
    let mut g = Graph::new();
    let a = g.input("a", 1.0).unwrap();
    let b = g.input("b", 2.0).unwrap();
    let whole = g.input("whole", 3_i64).unwrap();
    let label = g.define("label", text(a)).unwrap();
    let tagged = g
        .define(
            "tagged",
            when(label.equals("1.0".to_string())).then(a).otherwise(b),
        )
        .unwrap();
    let sum = g.define("sum", a + b).unwrap();
    g.stabilize();

    // Text made from a varied input.
    let refused = Scenarios::new(&g, &[a.id()], &[tagged.id()]).unwrap_err();
    assert!(
        matches!(&refused, PlanError::NotAFloat { node, .. } if node == "label"),
        "{refused}"
    );
    // The same node is fine when nothing it reads is varied: it is a constant.
    let other = g.input("other", 4.0).unwrap();
    let plan = Scenarios::new(&g, &[other.id()], &[tagged.id()]).unwrap();
    assert_eq!(plan.run_cpu(&[9.0]), [1.0]);

    assert!(matches!(
        Scenarios::new(&g, &[sum.id()], &[sum.id()]),
        Err(PlanError::NotAnInput(_))
    ));
    assert!(matches!(
        Scenarios::new(&g, &[whole.id()], &[sum.id()]),
        Err(PlanError::NotAFloat { .. })
    ));
    assert!(matches!(
        Scenarios::new(&g, &[a.id(), a.id()], &[sum.id()]),
        Err(PlanError::Repeated(_))
    ));
    assert!(matches!(
        Scenarios::with_shared(&g, &[a.id()], &[a.id()], &[sum.id()]),
        Err(PlanError::Repeated(_))
    ));
    assert!(matches!(
        Scenarios::new(&g, &[], &[sum.id()]),
        Err(PlanError::NoInputs)
    ));
    assert!(matches!(
        Scenarios::new(&g, &[a.id()], &[]),
        Err(PlanError::NoOutputs)
    ));
}

/// Forty random totals (a sum, a minimum, a maximum and a count of the same members),
/// each over up to seventy members whose values are varied, shared or the
/// same always, and which are present always, never, or in only some
/// scenarios. The plan must give the graph's totals bit for bit: it combines
/// the members in the tree the graph keeps.
fn totals_give_what_the_graph_gives(run: impl Fn(&Scenarios, &[f64]) -> Vec<f64>) {
    totals_give_what_the_graph_gives_in(false, run);
}

/// As above, with the plan's nodes in clusters if `clusters`.
fn totals_give_what_the_graph_gives_in(
    clusters: bool,
    run: impl Fn(&Scenarios, &[f64]) -> Vec<f64>,
) {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
        let mut g = Graph::new();
        let vary: Vec<Input<f64>> = (0..4)
            .map(|k| g.input(&format!("v{k}"), rng.value()).unwrap())
            .collect();
        let shared: Vec<Input<f64>> = (0..2)
            .map(|k| g.input(&format!("s{k}"), rng.value()).unwrap())
            .collect();
        let mut members = Vec::new();
        for m in 0..rng.below(71) {
            let value = match rng.below(4) {
                0 => g.input(&format!("fixed{m}"), rng.value()).unwrap().id(),
                1 => {
                    let s = shared[rng.below(shared.len())];
                    g.define(&format!("x{m}"), s * rng.value()).unwrap().id()
                }
                _ => {
                    let v = vary[rng.below(vary.len())];
                    g.define(&format!("x{m}"), v + rng.value()).unwrap().id()
                }
            };
            let present = match rng.below(5) {
                0 => g.input(&format!("p{m}"), false).unwrap().id(),
                1 => {
                    let v = vary[rng.below(vary.len())];
                    g.define(&format!("p{m}"), v.gt(0.0)).unwrap().id()
                }
                _ => g.input(&format!("p{m}"), true).unwrap().id(),
            };
            members.push((value, present));
        }
        let ops = [
            AggregateOp::Sum,
            AggregateOp::Min,
            AggregateOp::Max,
            AggregateOp::Count,
        ];
        let mut outputs: Vec<NodeId> = ops
            .iter()
            .map(|&op| {
                g.aggregate(&format!("{op}"), op, Type::F64, 0, &members)
                    .unwrap()
            })
            .collect();
        outputs.push(vary[0].id());
        g.stabilize();

        let make = match clusters {
            true => Scenarios::with_clusters,
            false => Scenarios::with_shared,
        };
        let mut plan = make(&g, &ids(&vary), &ids(&shared), &outputs).unwrap();
        for round in 0..3 {
            if round > 0 {
                let values: Vec<f64> = shared.iter().map(|_| rng.value()).collect();
                plan.set_shared(&values);
                for (&input, &x) in shared.iter().zip(&values) {
                    g.set(input, x).unwrap();
                }
            }
            let inputs: Vec<f64> = (0..40 * vary.len()).map(|_| rng.value()).collect();
            let got = run(&plan, &inputs);
            let want = by_the_graph(&mut g, &vary, &outputs, &inputs);
            assert_same(&format!("seed {seed}, round {round}"), &got, &want);
        }
    }
}

#[test]
fn on_the_cpu_a_total_gives_what_the_graph_gives_bit_for_bit() {
    totals_give_what_the_graph_gives(|plan, inputs| plan.run_cpu(inputs));
}

#[test]
fn on_the_cpu_a_total_in_clusters_gives_what_the_graph_gives() {
    totals_give_what_the_graph_gives_in(true, |plan, inputs| plan.run_cpu(inputs));
}

/// A collection of `rows` rows whose `line` column reads the varied `fx`, and
/// its sum, largest and smallest line, then a node that reads the sum.
fn orders(rows: usize) -> (Graph, Input<f64>, Vec<NodeId>) {
    let mut g = Graph::new();
    let fx = g.input("fx", 1.25).unwrap();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty").unwrap();
    let price = orders.field::<f64>("price").unwrap();
    let line = orders.column(&mut g, "line", qty * price * fx).unwrap();
    let revenue = orders.sum(&mut g, "revenue", line).unwrap();
    let largest = orders.max(&mut g, "largest", line).unwrap();
    let smallest = orders.min(&mut g, "smallest", line).unwrap();
    for i in 0..rows {
        let row = [
            qty.is((i % 37) as f64 + 0.5),
            price.is(1.0 + (i % 11) as f64 / 7.0),
        ];
        orders.upsert(&mut g, &format!("o{i}"), &row).unwrap();
    }
    g.stabilize();
    let after = g.define("after", revenue * 2.0 + fx).unwrap();
    g.stabilize();
    let outputs = vec![revenue.id(), largest.id(), smallest.id(), after.id()];
    (g, fx, outputs)
}

#[test]
fn a_long_total_gives_what_the_graph_gives_on_the_cpu() {
    let (mut g, fx, outputs) = orders(2000);
    let plan = Scenarios::new(&g, &[fx.id()], &outputs).unwrap();
    let inputs = [0.5, 1.0, -3.0, f64::NAN, 0.0, -0.0, 1e300, 7.25];
    let want = by_the_graph(&mut g, &[fx], &outputs, &inputs);
    assert_same("a long total", &plan.run_cpu(&inputs), &want);
}

#[test]
fn a_collections_totals_run_as_a_plan_and_can_be_read_by_what_follows() {
    let mut g = Graph::new();
    let fx = g.input("fx", 1.25).unwrap();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty").unwrap();
    let price = orders.field::<f64>("price").unwrap();
    let line = orders.column(&mut g, "line", qty * price * fx).unwrap();
    let revenue = orders.sum(&mut g, "revenue", line).unwrap();
    let largest = orders.max(&mut g, "largest", line).unwrap();
    let smallest = orders.min(&mut g, "smallest", line).unwrap();
    let count = orders.count(&mut g, "count").unwrap();
    for i in 0..1000 {
        let row = [
            qty.is((i % 37) as f64 + 0.5),
            price.is(1.0 + (i % 11) as f64 / 7.0),
        ];
        orders.upsert(&mut g, &format!("o{i}"), &row).unwrap();
    }
    g.stabilize();
    // A removed row is absent from the totals.
    orders.remove(&mut g, "o7").unwrap();
    let after = g.define("after", revenue * 2.0 + fx).unwrap();
    g.stabilize();

    let outputs = [revenue.id(), largest.id(), smallest.id(), after.id()];
    let plan = Scenarios::new(&g, &[fx.id()], &outputs).unwrap();
    let inputs = [0.5, 1.0, -3.0, f64::NAN, 0.0, -0.0, 1e300];
    let got = plan.run_cpu(&inputs);
    let want = by_the_graph(&mut g, &[fx], &outputs, &inputs);
    assert_same("a collection's totals", &got, &want);

    // Saved and read back, it gives the same.
    let mut file = Vec::new();
    plan.save(&mut file).unwrap();
    let back = Scenarios::load(&file[..]).unwrap();
    assert_same("read back", &back.run_cpu(&inputs), &want);

    // A count is a whole number, which comes out as its f64. Here no row
    // comes or goes with `fx`, so it is the same in every scenario.
    let plan = Scenarios::new(&g, &[fx.id()], &[count.id()]).unwrap();
    assert_eq!(plan.run_cpu(&[0.5, 2.0]), [999.0, 999.0]);
}

#[test]
fn a_large_total_is_worked_out_in_pieces_and_keeps_no_member() {
    // 100,000 rows. Each line is read only by the three totals, so it is
    // worked out inside them rather than kept, and each total is cut into
    // pieces of whole subtrees, short enough for a kernel each.
    let rows = 100_000;
    let (mut g, fx, outputs) = orders(rows);
    let plan = Scenarios::new(&g, &[fx.id()], &outputs).unwrap();
    let inputs = [0.5, 1.0, -3.0, f64::NAN, 0.0, -0.0, 1e300, 7.25];
    let want = by_the_graph(&mut g, &[fx], &outputs, &inputs);
    assert_same("a large total", &plan.run_cpu(&inputs), &want);
    // Pieces of a total, not one node each; and far fewer slots than rows.
    assert!(plan.nodes() > 3 * 100, "{} nodes", plan.nodes());
    assert!(
        plan.bytes_per_scenario() < rows * 8 / 10,
        "{} bytes a scenario",
        plan.bytes_per_scenario()
    );
    // Read back from a file, it gives the same.
    let mut file = Vec::new();
    plan.save(&mut file).unwrap();
    let back = Scenarios::load(&file[..]).unwrap();
    assert_same("read back", &back.run_cpu(&inputs), &want);
}

/// Orders grouped by whether their line is over 40, which changes with
/// `fx`: per group, how many there are and their sum, and what is made from
/// them, a count among it.
fn grouped_by_a_varied_column() -> (Graph, Input<f64>, Node<i64>, Vec<NodeId>) {
    let mut g = Graph::new();
    let fx = g.input("fx", 1.0).unwrap();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty").unwrap();
    let price = orders.field::<f64>("price").unwrap();
    let line = orders.column(&mut g, "line", qty * price * fx).unwrap();
    let big = orders.column(&mut g, "big", line.gt(40.0)).unwrap();
    let counts = orders.group_count(&mut g, "how many", big).unwrap();
    let sums = orders.group_sum(&mut g, "worth", big, line).unwrap();
    for i in 0..300 {
        let row = [
            qty.is((i % 13) as f64 + 0.5),
            price.is(1.0 + (i % 7) as f64),
        ];
        orders.upsert(&mut g, &format!("o{i}"), &row).unwrap();
    }
    // Some scenarios put every row in one group, so both groups are made.
    g.set(fx, 1000.0).unwrap();
    g.stabilize();
    g.set(fx, 0.001).unwrap();
    g.stabilize();
    g.set(fx, 1.0).unwrap();
    g.stabilize();
    let many = orders.group(&g, counts, true).unwrap();
    let few = orders.group(&g, counts, false).unwrap();
    let worth = orders.group(&g, sums, true).unwrap();
    // Compared, and the smaller taken, as whole numbers.
    let mostly_big = g.define("mostly big", many.gt(few)).unwrap();
    let smaller = g.define("smaller group", many.min(few)).unwrap();
    let average = g
        .define(
            "average big",
            when(mostly_big).then(worth / 2.0).otherwise(fx),
        )
        .unwrap();
    g.stabilize();
    let outputs = vec![many.id(), few.id(), smaller.id(), worth.id(), average.id()];
    (g, fx, many, outputs)
}

#[test]
fn a_count_gives_what_the_graph_gives() {
    let (mut g, fx, _, outputs) = grouped_by_a_varied_column();
    let plan = Scenarios::new(&g, &[fx.id()], &outputs).unwrap();
    let inputs = [0.5, 1.0, -3.0, f64::NAN, 0.0, 2.5, 1e300, 7.25, 0.01];
    let want = by_the_graph(&mut g, &[fx], &outputs, &inputs);
    // The groups' counts do change from one scenario to another.
    let many: Vec<f64> = want.iter().step_by(outputs.len()).copied().collect();
    assert!(many.iter().any(|&n| n != many[0]), "{many:?}");
    assert_same("counts", &plan.run_cpu(&inputs), &want);
    assert_same(
        "counts in clusters",
        &Scenarios::with_clusters(&g, &[fx.id()], &[], &outputs)
            .unwrap()
            .run_cpu(&inputs),
        &want,
    );
}

#[test]
fn group_totals_whose_rows_came_and_went_give_what_the_graph_gives() {
    // Rows come and go in groups, so the groups' totals have members that
    // were emptied and filled again by other rows; a plan follows the
    // totals as they are, holes and all.
    let mut g = Graph::new();
    let fx = g.input("fx", 1.25).unwrap();
    let mut window = Collection::new(&g, "window");
    let group = window.field::<String>("group").unwrap();
    let amount = window.field::<f64>("amount").unwrap();
    let value = window.column(&mut g, "value", amount * fx).unwrap();
    let by_group = window.group_sum(&mut g, "value", group, value).unwrap();
    let counts = window.group_count(&mut g, "rows", group).unwrap();
    let mut rng = Rng(11);
    for i in 0..3000 {
        let row = [
            group.is(format!("g{}", rng.below(12))),
            amount.is(rng.value()),
        ];
        window.upsert(&mut g, &format!("r{i}"), &row).unwrap();
        if i >= 200 {
            window.remove(&mut g, &format!("r{}", i - 200)).unwrap();
        }
        g.stabilize();
    }
    let mut outputs = Vec::new();
    for key in window.group_keys(&g, by_group) {
        outputs.push(window.group(&g, by_group, key.clone()).unwrap().id());
        outputs.push(window.group(&g, counts, key).unwrap().id());
    }
    // Some members were emptied: they read the graph's always-false node.
    let hole = g.find("(no member)").expect("a member was emptied");
    let holes = outputs
        .iter()
        .filter_map(|&id| g.aggregate_members(id))
        .flat_map(|(_, members)| members)
        .filter(|&(_, present)| present == hole)
        .count();
    assert!(holes > 0);
    let plan = Scenarios::new(&g, &[fx.id()], &outputs).unwrap();
    let inputs = [0.5, 1.0, -3.0, f64::NAN, 0.0, -0.0, 1e300, 7.25];
    let want = by_the_graph(&mut g, &[fx], &outputs, &inputs);
    assert_same("group totals after churn", &plan.run_cpu(&inputs), &want);
}

#[test]
fn a_choice_by_a_number_the_same_in_every_scenario_takes_that_option() {
    let mut g = Graph::new();
    let a = g.input("a", 1.0).unwrap();
    let b = g.input("b", 2.0).unwrap();
    let source = g.input("source", 1_i64).unwrap();
    let picked = g
        .define("picked", choose(source, [a * 2.0, a + b, b * b]))
        .unwrap();
    let after = g.define("after", picked * 10.0 + a).unwrap();
    g.stabilize();
    let outputs = [picked.id(), after.id()];
    for k in 0..3_i64 {
        g.set(source, k).unwrap();
        g.stabilize();
        let plan = Scenarios::with_shared(&g, &[a.id()], &[b.id()], &outputs).unwrap();
        let inputs = [0.5, -1.0, f64::NAN, 3.0];
        let want = by_the_graph(&mut g, &[a], &outputs, &inputs);
        assert_same(&format!("option {k}"), &plan.run_cpu(&inputs), &want);
    }

    // Out of range, the node holds an error in every scenario.
    g.set(source, 3).unwrap();
    g.stabilize();
    let refused = Scenarios::new(&g, &[a.id()], &outputs).unwrap_err();
    assert!(
        matches!(&refused, PlanError::HoldsError { node, .. } if node == "picked"),
        "{refused}"
    );
}

#[test]
fn a_choice_by_a_number_that_varies_is_refused_by_name() {
    // Which option a count picks changes with `fx`; the graph would hold an
    // error in some scenarios, which a plan cannot.
    let (mut g, fx, many, _) = grouped_by_a_varied_column();
    let picked = g
        .define("picked", choose(many, [fx * 1.0, fx * 2.0]))
        .unwrap();
    g.stabilize();
    let refused = Scenarios::new(&g, &[fx.id()], &[picked.id()]).unwrap_err();
    assert!(
        matches!(&refused, PlanError::Unsupported { node, what } if node == "picked" && what.contains("varies")),
        "{refused}"
    );
}

#[test]
fn an_expression_deeper_than_the_stack_is_split_into_nodes() {
    // A lookup into a table of 300 rows, as a spreadsheet's VLOOKUP is
    // written: each row a condition around the rows after it, which needs a
    // stack two deeper a row.
    let mut g = Graph::new();
    let x = g.input("x", 1.0).unwrap();
    let mut found: Expr<f64> = f64::NAN.into_expr();
    for row in (0..300).rev() {
        let key = row as f64 * 0.5;
        found = when(x.ge(key)).then(key * key + 1.0).otherwise(found);
    }
    let lookup = g.define("lookup", found).unwrap();
    let after = g.define("after", lookup * x).unwrap();
    g.stabilize();
    let outputs = [lookup.id(), after.id()];
    let plan = Scenarios::new(&g, &[x.id()], &outputs).unwrap();
    // About fifteen rows a node: each row needs two more of the stack.
    assert!((15..=30).contains(&plan.nodes()), "{} nodes", plan.nodes());
    let inputs = [-1.0, 0.0, 0.25, 0.5, 7.0, 149.5, 1000.0, f64::NAN];
    let want = by_the_graph(&mut g, &[x], &outputs, &inputs);
    assert_same("a deep lookup", &plan.run_cpu(&inputs), &want);
}

/// Forty random graphs, each run once with some of its nodes kept, then
/// brought up to date after rounds of changes to the inputs built into the
/// plan, naming every changed input, some of them, or none: every kept value
/// must be the same to the last bit as making the plan again and running it.
#[test]
fn a_refresh_gives_what_making_and_running_the_plan_again_gives() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
        let mut r = random_graph(&mut rng, 14, 300);
        let vary = ids(&r.inputs[..6]);
        let shared = ids(&r.inputs[6..8]);
        let built_in: Vec<Input<f64>> = r.inputs[8..].to_vec();
        let mut outputs: Vec<NodeId> = (0..6)
            .map(|_| r.nodes[rng.below(r.nodes.len())].id())
            .collect();
        outputs.push(r.nodes.last().unwrap().id());
        // Sometimes the nodes nearest the outputs, sometimes nodes anywhere.
        let keep: Vec<NodeId> = match seed % 2 {
            0 => Kept::nearest(&r.g, &outputs, rng.below(120)),
            _ => (0..rng.below(60))
                .map(|_| r.nodes[rng.below(r.nodes.len())].id())
                .collect(),
        };
        let mut kept = Kept::new(&r.g, &vary, &shared, &outputs, &keep).unwrap();
        let inputs: Vec<f64> = (0..30 * vary.len()).map(|_| rng.value()).collect();
        let results = kept.plan().run_cpu(&inputs);
        kept.hold(inputs.clone(), results);

        for round in 0..6 {
            let mut changed = Vec::new();
            for _ in 0..1 + rng.below(3) {
                let input = built_in[rng.below(built_in.len())];
                // Now and then to the value it has: no change at all.
                let x = match rng.below(5) {
                    0 => r.g.get(input).unwrap(),
                    _ => rng.value(),
                };
                r.g.set(input, x).unwrap();
                changed.push(input.id());
            }
            r.g.stabilize();
            let named: Vec<NodeId> = match round % 3 {
                0 => changed.clone(),
                1 => changed[..1].to_vec(),
                _ => Vec::new(),
            };
            kept.refresh_cpu(&r.g, &named).unwrap();
            let fresh = Kept::new(&r.g, &vary, &shared, &outputs, &keep).unwrap();
            let want = fresh.plan().run_cpu(&inputs);
            assert_same(&format!("seed {seed}, round {round}"), kept.values(), &want);
            assert!(kept.changed(&r.g).is_empty(), "seed {seed}, round {round}");
        }
    }
}

#[test]
fn a_refresh_after_the_graphs_shape_changed_is_refused() {
    let mut r = random_graph(&mut Rng(77), 6, 40);
    let vary = ids(&r.inputs[..2]);
    let output = r.nodes.last().unwrap().id();
    let mut kept = Kept::new(&r.g, &vary, &[], &[output], &[]).unwrap();
    let inputs = vec![0.5, 1.5];
    let results = kept.plan().run_cpu(&inputs);
    kept.hold(inputs, results);
    // A value changing is what a refresh is for.
    r.g.set(r.inputs[4], 9.0).unwrap();
    r.g.stabilize();
    assert!(kept.refresh_cpu(&r.g, &[]).is_ok());
    // What a node reads changing is not: the plan is made again.
    let input = r.inputs[5];
    r.g.redefine(r.nodes[3], input * 2.0).unwrap();
    r.g.stabilize();
    assert_eq!(kept.refresh_cpu(&r.g, &[]), Err(PlanError::Reshaped));
    assert_eq!(
        kept.patch(&r.g, &[]).map(|p| p.is_some()),
        Err(PlanError::Reshaped)
    );
}

/// Needs an NVIDIA GPU and the CUDA libraries: `cargo test --features cuda`.
#[cfg(feature = "gpu")]
mod on_the_gpu {
    use super::*;
    use watershed_gpu::Gpu;

    #[test]
    fn the_gpu_gives_what_the_graph_gives() {
        let gpu = Gpu::new().unwrap();
        gives_what_the_graph_gives(|plan, inputs| gpu.run(plan, inputs).unwrap());
    }

    #[test]
    fn clusters_give_what_the_graph_gives() {
        let gpu = Gpu::new().unwrap();
        gives_what_the_graph_gives_in(true, |plan, inputs| gpu.run(plan, inputs).unwrap());
        // And on the CPU, and worked out a node at a time on the GPU.
        gives_what_the_graph_gives_in(true, |plan, inputs| plan.run_cpu(inputs));
        let gpu = gpu.without_fusing();
        gives_what_the_graph_gives_in(true, |plan, inputs| gpu.run(plan, inputs).unwrap());
    }

    #[test]
    fn clusters_read_back_from_a_file_give_what_the_graph_gives() {
        let gpu = Gpu::new().unwrap();
        gives_what_the_graph_gives_in(true, |plan, inputs| {
            let mut file = Vec::new();
            plan.save(&mut file).unwrap();
            gpu.run(&Scenarios::load(&file[..]).unwrap(), inputs)
                .unwrap()
        });
    }

    #[test]
    fn on_the_gpu_a_total_gives_what_the_graph_gives_bit_for_bit() {
        let gpu = Gpu::new().unwrap();
        totals_give_what_the_graph_gives(|plan, inputs| gpu.run(plan, inputs).unwrap());
        // And by the general kernel.
        let gpu = gpu.without_compiling();
        totals_give_what_the_graph_gives(|plan, inputs| gpu.run(plan, inputs).unwrap());
    }

    #[test]
    fn a_total_in_clusters_gives_what_the_graph_gives() {
        let gpu = Gpu::new().unwrap();
        totals_give_what_the_graph_gives_in(true, |plan, inputs| gpu.run(plan, inputs).unwrap());
    }

    #[test]
    fn a_large_total_is_worked_out_in_pieces_on_the_gpu() {
        // 100,000 rows: each total is pieces of whole subtrees, each short
        // enough for a kernel of its own, worked out side by side.
        let (mut g, fx, outputs) = orders(100_000);
        let plan = Scenarios::new(&g, &[fx.id()], &outputs).unwrap();
        let inputs = [0.5, 1.0, -3.0, f64::NAN, 0.0, -0.0, 1e300, 7.25];
        let want = by_the_graph(&mut g, &[fx], &outputs, &inputs);
        let gpu = Gpu::new().unwrap();
        assert_same(
            "a large total on the GPU",
            &gpu.run(&plan, &inputs).unwrap(),
            &want,
        );
        let clusters = Scenarios::with_clusters(&g, &[fx.id()], &[], &outputs).unwrap();
        assert_same("in clusters", &gpu.run(&clusters, &inputs).unwrap(), &want);
    }

    #[test]
    fn on_the_gpu_a_count_gives_what_the_graph_gives() {
        let (mut g, fx, _, outputs) = grouped_by_a_varied_column();
        let plan = Scenarios::new(&g, &[fx.id()], &outputs).unwrap();
        let inputs = [0.5, 1.0, -3.0, f64::NAN, 0.0, 2.5, 1e300, 7.25, 0.01];
        let want = by_the_graph(&mut g, &[fx], &outputs, &inputs);
        let gpu = Gpu::new().unwrap();
        assert_same(
            "counts on the GPU",
            &gpu.run(&plan, &inputs).unwrap(),
            &want,
        );
    }

    #[test]
    fn on_the_gpu_a_deep_expression_split_into_nodes_gives_what_the_graph_gives() {
        let mut g = Graph::new();
        let x = g.input("x", 1.0).unwrap();
        let mut found: Expr<f64> = f64::NAN.into_expr();
        for row in (0..300).rev() {
            let key = row as f64 * 0.5;
            found = when(x.ge(key)).then(key * key + 1.0).otherwise(found);
        }
        let lookup = g.define("lookup", found).unwrap();
        g.stabilize();
        let plan = Scenarios::new(&g, &[x.id()], &[lookup.id()]).unwrap();
        let inputs = [-1.0, 0.0, 0.25, 0.5, 7.0, 149.5, 1000.0, f64::NAN];
        let want = by_the_graph(&mut g, &[x], &[lookup.id()], &inputs);
        let gpu = Gpu::new().unwrap();
        assert_same(
            "a deep lookup on the GPU",
            &gpu.run(&plan, &inputs).unwrap(),
            &want,
        );
    }

    #[test]
    fn the_general_kernel_gives_what_the_graph_gives() {
        let gpu = Gpu::new().unwrap().without_compiling();
        gives_what_the_graph_gives(|plan, inputs| gpu.run(plan, inputs).unwrap());
    }

    #[test]
    fn loaded_scenarios_give_the_same_again_and_with_other_shared_values() {
        let gpu = Gpu::new().unwrap();
        let mut rng = Rng(11);
        let r = random_graph(&mut rng, 14, 300);
        let outputs: Vec<NodeId> = r.nodes[280..].iter().map(|n| n.id()).collect();
        let mut plan =
            Scenarios::with_shared(&r.g, &ids(&r.inputs[..8]), &ids(&r.inputs[8..12]), &outputs)
                .unwrap();
        let inputs: Vec<f64> = (0..3000 * 8).map(|_| rng.value()).collect();
        let mut loaded = gpu.load(&plan, &inputs).unwrap();
        for _ in 0..3 {
            assert_same(
                "loaded",
                &loaded.run(&plan).unwrap(),
                &plan.run_cpu(&inputs),
            );
            // Added up on the GPU as on the CPU, to the last bit.
            assert_same(
                "totals",
                &loaded.totals(&plan).unwrap(),
                &plan.totals_cpu(&inputs),
            );
            let shared: Vec<f64> = (0..4).map(|_| rng.value()).collect();
            plan.set_shared(&shared);
        }
        // Other inputs for the same scenarios.
        let others: Vec<f64> = (0..3000 * 8).map(|_| rng.value()).collect();
        loaded.set_inputs(&plan, &others).unwrap();
        assert_same(
            "other inputs",
            &loaded.run(&plan).unwrap(),
            &plan.run_cpu(&others),
        );
    }

    #[test]
    fn maths_functions_are_within_a_few_last_digits_of_the_cpu() {
        let gpu = Gpu::new().unwrap();
        let (plan, _, _, _, inputs) = with_maths();
        let on_cpu = plan.run_cpu(&inputs);
        let on_gpu = gpu.run(&plan, &inputs).unwrap();
        for (cpu, gpu) in on_cpu.iter().zip(&on_gpu) {
            assert!((cpu - gpu).abs() <= 1e-14 * cpu.abs(), "{cpu} and {gpu}");
        }
    }

    #[test]
    fn a_run_too_big_for_the_memory_allowed_is_done_in_parts() {
        let gpu = Gpu::new().unwrap();
        let mut rng = Rng(7);
        let r = random_graph(&mut rng, 14, 300);
        let outputs: Vec<NodeId> = r.nodes[250..].iter().map(|n| n.id()).collect();
        let plan = Scenarios::new(&r.g, &ids(&r.inputs[..8]), &outputs).unwrap();
        let inputs: Vec<f64> = (0..50 * 8).map(|_| rng.value()).collect();
        // Room for some of the fifty scenarios, not all.
        let (got, timing) = gpu
            .run_within(&plan, &inputs, 20 * plan.bytes_per_scenario())
            .unwrap();
        assert!((1..50).contains(&timing.scenarios_at_a_time), "{timing:?}");
        assert_same("in parts", &got, &plan.run_cpu(&inputs));
    }

    #[test]
    fn more_outputs_than_come_back_in_one_piece_are_each_in_their_place() {
        // Outputs are gathered on the GPU and copied back at most 2²²
        // numbers at a time. 800,000 scenarios of six outputs are 4.8
        // million numbers: two pieces, the second begun part-way through
        // the scenarios. Each scenario's input is its own, so a number in
        // another scenario's place, or another output's, is not the CPU's.
        let mut g = Graph::new();
        let a = g.input("a", 1.0).unwrap();
        let data = g.input("data", 3.0).unwrap();
        let rate = g.input("rate", 0.25).unwrap();
        let doubled = g.define("doubled", data * 2.0).unwrap();
        let fixed = g.define("fixed", rate * 10.0).unwrap();
        let sum = g.define("sum", a + doubled).unwrap();
        let square = g.define("square", a * a).unwrap();
        let less = g.define("less", a - fixed).unwrap();
        let halved = g.define("halved", sum * 0.5).unwrap();
        g.stabilize();
        // Four that are worked out for each scenario, with one that is the
        // same for all of them and one that never changes among them.
        let outputs = [
            sum.id(),
            doubled.id(),
            square.id(),
            fixed.id(),
            less.id(),
            halved.id(),
        ];
        let mut plan = Scenarios::with_shared(&g, &[a.id()], &[data.id()], &outputs).unwrap();
        let scenarios = 800_000;
        assert!(scenarios * outputs.len() > 1 << 22);
        let inputs: Vec<f64> = (0..scenarios).map(|k| k as f64 * 0.5 - 1000.0).collect();
        let gpu = Gpu::new().unwrap();
        assert_same(
            "in several pieces",
            &gpu.run(&plan, &inputs).unwrap(),
            &plan.run_cpu(&inputs),
        );
        // And from a plan kept on the GPU, again with another shared value.
        let mut loaded = gpu.load(&plan, &inputs).unwrap();
        for shared in [3.0, -7.5] {
            plan.set_shared(&[shared]);
            assert_same(
                "loaded, in several pieces",
                &loaded.run(&plan).unwrap(),
                &plan.run_cpu(&inputs),
            );
        }
    }
}
