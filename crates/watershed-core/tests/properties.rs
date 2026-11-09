//! Property tests: instead of hand-picked examples, these build thousands of
//! random graphs, apply random updates, and after every update check rules
//! that must always hold.
//!
//! The central rule is the simplest possible definition of "correct":
//! the values the engine reaches by incremental updates must be identical to
//! recalculating every node from scratch.
//!
//! When a property fails, proptest prints the smallest case it can find.

use std::collections::HashSet;
use std::sync::Arc;

use watershed_core::{BinOp, Graph, Ir, NodeId, Type, UnOp, Value};
use proptest::prelude::*;

/// A small deterministic random number generator (SplitMix64), seeded by proptest.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn percent(&mut self, p: u64) -> bool {
        self.next() % 100 < p
    }
    fn pick<T: Clone>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())].clone()
    }
}

const TYPES: [Type; 4] = [Type::F64, Type::I64, Type::Bool, Type::Str];

/// A random value. The choices are deliberately small, so that values often
/// repeat (which exercises cutoff), with awkward ones mixed in: NaN, -0.0,
/// huge numbers and integers at the edge of overflow.
fn random_value(rng: &mut Rng, ty: Type) -> Value {
    match ty {
        Type::F64 => Value::F64(rng.pick(&[
            0.0,
            1.0,
            -1.0,
            0.5,
            2.0,
            -0.0,
            1e308,
            f64::NAN,
            f64::INFINITY,
        ])),
        Type::I64 => Value::I64(rng.pick(&[-2, -1, 0, 1, 2, 3, i64::MAX, i64::MIN])),
        Type::Bool => Value::Bool(rng.percent(50)),
        Type::Str => Value::Str(rng.pick(&["", "a", "b", "ab"]).into()),
    }
}

/// A random, well-typed expression producing `ty`, using existing nodes.
fn random_expr(rng: &mut Rng, g: &Graph, ty: Type, depth: u32) -> Ir {
    if depth == 0 || rng.percent(30) {
        let candidates: Vec<NodeId> = g.node_ids().filter(|&id| g.node_type(id) == ty).collect();
        if !candidates.is_empty() && rng.percent(80) {
            return g.reference(rng.pick(&candidates));
        }
        return Ir::Const(random_value(rng, ty));
    }
    let sub = |rng: &mut Rng, ty: Type| Arc::new(random_expr(rng, g, ty, depth - 1));
    match ty {
        Type::F64 | Type::I64 => match rng.below(3) {
            0 => {
                let op = rng.pick(&[
                    BinOp::Add,
                    BinOp::Sub,
                    BinOp::Mul,
                    BinOp::Div,
                    BinOp::Min,
                    BinOp::Max,
                ]);
                Ir::Binary(op, sub(rng, ty), sub(rng, ty))
            }
            1 => Ir::Unary(rng.pick(&[UnOp::Neg, UnOp::Abs]), sub(rng, ty)),
            _ => Ir::If(sub(rng, Type::Bool), sub(rng, ty), sub(rng, ty)),
        },
        Type::Bool => match rng.below(4) {
            0 => {
                let operand = rng.pick(&[Type::F64, Type::I64, Type::Str]);
                let op = rng.pick(&[
                    BinOp::Lt,
                    BinOp::Le,
                    BinOp::Gt,
                    BinOp::Ge,
                    BinOp::Eq,
                    BinOp::Ne,
                ]);
                Ir::Binary(op, sub(rng, operand), sub(rng, operand))
            }
            1 => Ir::Binary(
                rng.pick(&[BinOp::And, BinOp::Or]),
                sub(rng, Type::Bool),
                sub(rng, Type::Bool),
            ),
            2 => Ir::Unary(UnOp::Not, sub(rng, Type::Bool)),
            _ => Ir::If(
                sub(rng, Type::Bool),
                sub(rng, Type::Bool),
                sub(rng, Type::Bool),
            ),
        },
        Type::Str => Ir::If(
            sub(rng, Type::Bool),
            sub(rng, Type::Str),
            sub(rng, Type::Str),
        ),
    }
}

/// A random expression that ignores types altogether, to throw nonsense at
/// the type checker.
fn random_untyped_expr(rng: &mut Rng, g: &Graph, depth: u32) -> Ir {
    if depth == 0 || rng.percent(30) {
        let ty = rng.pick(&TYPES);
        return random_expr(rng, g, ty, 0);
    }
    let sub = |rng: &mut Rng| Arc::new(random_untyped_expr(rng, g, depth - 1));
    match rng.below(3) {
        0 => Ir::Unary(rng.pick(&[UnOp::Neg, UnOp::Abs, UnOp::Not]), sub(rng)),
        1 => {
            let op = rng.pick(&[
                BinOp::Add,
                BinOp::Sub,
                BinOp::Mul,
                BinOp::Div,
                BinOp::Min,
                BinOp::Max,
                BinOp::Lt,
                BinOp::Le,
                BinOp::Gt,
                BinOp::Ge,
                BinOp::Eq,
                BinOp::Ne,
                BinOp::And,
                BinOp::Or,
            ]);
            Ir::Binary(op, sub(rng), sub(rng))
        }
        _ => Ir::If(sub(rng), sub(rng), sub(rng)),
    }
}

/// Adds a random calculated node. Returns whether it was accepted.
fn add_random_node(rng: &mut Rng, g: &mut Graph, n: usize, nonsense: bool) -> bool {
    let name = format!("n{n}");
    if nonsense {
        let ir = random_untyped_expr(rng, g, 3);
        // May be refused, but must never panic, and if accepted the rest of
        // the test checks that its values always have its declared type.
        g.define_ir(&name, ir).is_ok()
    } else {
        let ty = rng.pick(&TYPES);
        let ir = random_expr(rng, g, ty, 3);
        let id = g
            .define_ir(&name, ir)
            .expect("a well-typed expression must be accepted");
        assert_eq!(g.node_type(id), ty);
        true
    }
}

/// Builds a random graph, then applies random updates. After each one it
/// checks every rule. Returns a description of the first rule broken.
fn run(
    seed: u64,
    inputs: usize,
    calculated: usize,
    updates: usize,
    nonsense: bool,
) -> Result<(), String> {
    let mut rng = Rng(seed);
    let mut g = Graph::new();
    let mut input_ids = Vec::new();
    for i in 0..inputs {
        let ty = rng.pick(&TYPES);
        input_ids.push(
            g.input_value(&format!("in{i}"), random_value(&mut rng, ty))
                .unwrap(),
        );
    }
    let mut n = 0;
    for _ in 0..calculated {
        add_random_node(&mut rng, &mut g, n, nonsense);
        n += 1;
    }
    g.check_consistency()?;

    for _ in 0..updates {
        // Sometimes set inputs and add nodes before the update, to check that
        // nodes added between updates join in correctly.
        let changes = 1 + rng.below(3);
        for _ in 0..changes {
            let id = rng.pick(&input_ids);
            let value = if rng.percent(10) {
                Value::Error("feed down".into())
            } else {
                random_value(&mut rng, g.node_type(id))
            };
            g.set_value(id, value).unwrap();
        }
        if rng.percent(20) {
            add_random_node(&mut rng, &mut g, n, nonsense);
            n += 1;
        }

        let before: Vec<Value> = g.node_ids().map(|id| g.value(id).clone()).collect();
        let report = g.stabilize();
        let changed: HashSet<NodeId> = g
            .node_ids()
            .filter(|&id| !g.value(id).same_as(&before[id.index()]))
            .collect();
        let recalculated: Vec<NodeId> = g.last_recalculated().to_vec();
        let recalculated_set: HashSet<NodeId> = recalculated.iter().copied().collect();

        // Rule 1: incremental values are identical to a from-scratch recalculation.
        g.check_consistency()?;

        // Rule 2: no node is recalculated twice in one update.
        if recalculated_set.len() != recalculated.len() {
            return Err(format!("a node was recalculated twice: {recalculated:?}"));
        }

        // Rule 3: exactly the nodes that use something that changed are
        // recalculated. Not one more (that would be wasted work), and not
        // one fewer (that would leave a stale value).
        for id in g.node_ids() {
            let uses_a_change = g.dependencies(id).iter().any(|d| changed.contains(d));
            let was_recalculated = recalculated_set.contains(&id);
            if uses_a_change != was_recalculated {
                return Err(format!(
                    "{}: uses a changed node = {uses_a_change}, but recalculated = {was_recalculated}",
                    g.name(id)
                ));
            }
        }

        // Rule 4: inputs are never recalculated, and the report adds up.
        if recalculated.iter().any(|&id| g.is_input(id)) {
            return Err("an input was recalculated".into());
        }
        let inputs_changed = changed.iter().filter(|&&id| g.is_input(id)).count();
        if report.inputs_changed != inputs_changed
            || report.recalculated != recalculated.len()
            || report.changed != changed.len() - inputs_changed
        {
            return Err(format!("report {report:?} does not match what happened"));
        }

        // Rule 5: a node is recalculated only after everything it uses.
        let position: std::collections::HashMap<NodeId, usize> = recalculated
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();
        for &id in &recalculated {
            for dep in g.dependencies(id) {
                if let Some(&p) = position.get(dep) {
                    if p > position[&id] {
                        return Err(format!(
                            "{} was recalculated before {}",
                            g.name(id),
                            g.name(*dep)
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn random_graphs_always_match_a_full_recalculation(
        seed in any::<u64>(),
        inputs in 1usize..8,
        calculated in 0usize..40,
        updates in 1usize..20,
    ) {
        run(seed, inputs, calculated, updates, false).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn nonsense_expressions_are_refused_or_behave_correctly(
        seed in any::<u64>(),
        inputs in 1usize..8,
        calculated in 0usize..40,
        updates in 1usize..10,
    ) {
        run(seed, inputs, calculated, updates, true).map_err(TestCaseError::fail)?;
    }
}

/// A few bigger graphs, so the properties are also checked at a size where
/// long chains and wide fan-out appear.
#[test]
fn larger_random_graphs_always_match_a_full_recalculation() {
    for seed in 0..20 {
        run(seed, 30, 2000, 50, false).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
    }
}
