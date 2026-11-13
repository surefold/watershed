//! Scenarios must give what the graph gives: for each scenario, set the
//! inputs on the graph, update it, and read the outputs.

use watershed_core::prelude::*;
use watershed_core::{NodeId, Value, text, when};
use watershed_gpu::{PlanError, Scenarios};

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
        .define("curve", (x * rate).exp() - (x + 1.0).ln() + x.sqrt())
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

/// Needs an NVIDIA GPU and the CUDA libraries: `cargo test --features cuda`.
#[cfg(feature = "cuda")]
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
}
