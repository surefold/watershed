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

use proptest::prelude::*;
use watershed_core::{AggregateOp, BinOp, CalcError, Graph, Ir, NodeId, Type, UnOp, Value};

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
    // A choice by position, of any type. Its index is often out of range,
    // which must be an error.
    if rng.percent(15) {
        let options = (0..1 + rng.below(3)).map(|_| sub(rng, ty)).collect();
        return Ir::Choose(sub(rng, Type::I64), options);
    }
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
    match rng.below(4) {
        3 => {
            let options = (0..rng.below(3)).map(|_| sub(rng)).collect();
            Ir::Choose(sub(rng), options)
        }
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

/// A random member for an aggregate over `value_type`: a value node and a
/// `bool` presence node, both existing. `None` if the graph has no fit.
fn random_member(
    rng: &mut Rng,
    g: &Graph,
    op: AggregateOp,
    value_type: Type,
) -> Option<(NodeId, NodeId)> {
    let of =
        |ty: Type| -> Vec<NodeId> { g.node_ids().filter(|&id| g.node_type(id) == ty).collect() };
    let presences = of(Type::Bool);
    if presences.is_empty() {
        return None;
    }
    let present = rng.pick(&presences);
    if op == AggregateOp::Count {
        return Some((present, present));
    }
    let values = of(value_type);
    if values.is_empty() {
        return None;
    }
    Some((rng.pick(&values), present))
}

/// Adds a random aggregate over random members, sitting a little above the
/// current graph so that later members can sometimes join it. Returns it,
/// with its operation and value type.
fn add_random_aggregate(rng: &mut Rng, g: &mut Graph, n: usize) -> (NodeId, AggregateOp, Type) {
    let op = rng.pick(&[
        AggregateOp::Sum,
        AggregateOp::Count,
        AggregateOp::Min,
        AggregateOp::Max,
    ]);
    let value_type = if op == AggregateOp::Count {
        Type::I64
    } else {
        rng.pick(&[Type::F64, Type::I64])
    };
    let members: Vec<(NodeId, NodeId)> = (0..rng.below(12))
        .filter_map(|_| random_member(rng, g, op, value_type))
        .collect();
    let top = g.node_ids().map(|id| g.height(id)).max().unwrap_or(0);
    let id = g
        .aggregate(
            &format!("agg{n}"),
            op,
            value_type,
            top + rng.below(3),
            &members,
        )
        .expect("a valid aggregate must be accepted");
    (id, op, value_type)
}

/// Adds a random calculated node. Returns whether it was accepted.
fn add_random_node(rng: &mut Rng, g: &mut Graph, n: usize, nonsense: bool) -> bool {
    let name = format!("n{n}");
    if nonsense {
        let ir = random_untyped_expr(rng, g, 3);
        // May be refused, but must never panic, and if accepted the rest of
        // the test checks that its values always have its declared type.
        match g.define_ir(&name, ir.clone()) {
            Ok(id) => {
                let rebuilt = g
                    .expression(id)
                    .expect("a calculated node has an expression");
                assert!(
                    rebuilt.same_as(&ir),
                    "compiling and rebuilding changed the expression"
                );
                true
            }
            Err(_) => false,
        }
    } else {
        let ty = rng.pick(&TYPES);
        let ir = random_expr(rng, g, ty, 3);
        let id = g
            .define_ir(&name, ir.clone())
            .expect("a well-typed expression must be accepted");
        // The consistency check evaluates expressions rebuilt from compiled
        // programs, so rebuilding must give back exactly what was compiled.
        let rebuilt = g
            .expression(id)
            .expect("a calculated node has an expression");
        assert!(
            rebuilt.same_as(&ir),
            "compiling and rebuilding changed the expression:\n  {ir:?}\n  {rebuilt:?}"
        );
        assert_eq!(g.node_type(id), ty);
        true
    }
}

/// Builds a random graph, then applies random updates. After each one it
/// checks every rule. Returns a description of the first rule broken.
/// With `parallel`, every level is recalculated on all cores, however small.
fn run(
    seed: u64,
    inputs: usize,
    calculated: usize,
    updates: usize,
    nonsense: bool,
    parallel: bool,
) -> Result<(), String> {
    let mut rng = Rng(seed);
    let mut g = Graph::new();
    if parallel {
        g.set_parallel_threshold(1);
    }
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
    // Totals over random members, some of which will grow during the updates.
    let aggregates: Vec<(NodeId, AggregateOp, Type)> = (0..rng.below(4))
        .map(|k| add_random_aggregate(&mut rng, &mut g, k))
        .collect();
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
        // Sometimes a member joins an aggregate. It may sit too high, which
        // must be refused cleanly; if accepted, it counts from this update.
        let mut joined: HashSet<NodeId> = HashSet::new();
        if !aggregates.is_empty() && rng.percent(40) {
            let (aggregate, op, value_type) = rng.pick(&aggregates);
            if let Some((value, present)) = random_member(&mut rng, &g, op, value_type) {
                match g.extend_aggregate(aggregate, value, present) {
                    Ok(()) => {
                        joined.insert(aggregate);
                    }
                    Err(CalcError::Invalid(_)) => {}
                    Err(other) => return Err(format!("extending an aggregate failed: {other}")),
                }
            }
        }

        let before: Vec<Value> = g.node_ids().map(|id| g.value(id).clone()).collect();
        // What each node actually read when it was last calculated.
        let reads_before: Vec<Vec<NodeId>> = g.node_ids().map(|id| g.reads(id)).collect();
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

        // Rule 3: exactly the nodes that read something that changed are
        // recalculated. Not one more (that would be wasted work), and not
        // one fewer (that would leave a stale value). "Read" means read when
        // the node was last calculated: a node with an `if` or a `choose`
        // reads only the branch or option it took, so a change elsewhere
        // cannot affect it.
        for id in g.node_ids() {
            let read_a_change = reads_before[id.index()].iter().any(|d| changed.contains(d));
            let was_recalculated = recalculated_set.contains(&id);
            // An aggregate that gained a member is recalculated to count it.
            let should = read_a_change || joined.contains(&id);
            if should != was_recalculated {
                return Err(format!(
                    "{}: read a changed node = {read_a_change}, but recalculated = {was_recalculated}",
                    g.name(id)
                ));
            }
            // A node only ever reads nodes its expression mentions.
            let mentions = g.dependencies(id);
            if let Some(stray) = g.reads(id).iter().find(|r| !mentions.contains(r)) {
                return Err(format!(
                    "{} read {}, which it does not mention",
                    g.name(id),
                    g.name(*stray)
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

        // Rule 6: the links agree in both directions. Each node lists every
        // user exactly once, and a node is a user of exactly what it uses.
        for id in g.node_ids() {
            let users = g.dependents(id);
            let unique: HashSet<NodeId> = users.iter().copied().collect();
            if unique.len() != users.len() {
                return Err(format!("{} lists a user twice: {users:?}", g.name(id)));
            }
            for user in users {
                if !g.dependencies(user).contains(&id) {
                    return Err(format!(
                        "{} lists {} as a user, but it is not",
                        g.name(id),
                        g.name(user)
                    ));
                }
            }
            for dep in g.dependencies(id) {
                if !g.dependents(dep).contains(&id) {
                    return Err(format!(
                        "{} uses {}, but is not listed as its user",
                        g.name(id),
                        g.name(dep)
                    ));
                }
            }
        }

        // Rule 7: min, max and count give the same answer in any order, so
        // they must equal a plain calculation over the present members,
        // exactly. (This does not share the aggregate's own combining code.)
        for &(id, _, _) in &aggregates {
            let (op, members) = g.aggregate_members(id).expect("an aggregate");
            let mut naive: Option<Value> = None;
            for (value, present) in members {
                let contribution = match (g.value(present), g.value(value)) {
                    (Value::Bool(false), _) => continue,
                    (Value::Error(e), _) => Value::Error(e.clone()),
                    (_, Value::Error(e)) if op != AggregateOp::Count => Value::Error(e.clone()),
                    (_, _) if op == AggregateOp::Count => Value::I64(1),
                    (_, v) => v.clone(),
                };
                naive = Some(match (naive, contribution) {
                    (None, c) => c,
                    (Some(Value::Error(e)), _) => Value::Error(e),
                    (Some(_), Value::Error(e)) => Value::Error(e),
                    // A NaN among the present values makes min and max NaN.
                    (Some(Value::F64(a)), Value::F64(_)) if a.is_nan() => Value::F64(a),
                    (Some(Value::F64(_)), Value::F64(b)) if b.is_nan() => Value::F64(b),
                    (Some(Value::F64(a)), Value::F64(b)) if op == AggregateOp::Min => {
                        Value::F64(a.min(b))
                    }
                    (Some(Value::F64(a)), Value::F64(b)) if op == AggregateOp::Max => {
                        Value::F64(a.max(b))
                    }
                    (Some(Value::I64(a)), Value::I64(b)) if op == AggregateOp::Min => {
                        Value::I64(a.min(b))
                    }
                    (Some(Value::I64(a)), Value::I64(b)) if op == AggregateOp::Max => {
                        Value::I64(a.max(b))
                    }
                    (Some(Value::I64(a)), Value::I64(b)) if op == AggregateOp::Count => {
                        Value::I64(a + b)
                    }
                    (Some(a), _) => a, // sums are checked by the consistency rule instead
                });
            }
            if op == AggregateOp::Sum {
                continue;
            }
            let naive = naive.unwrap_or_else(|| match (op, g.node_type(id)) {
                (AggregateOp::Count, _) => Value::I64(0),
                (AggregateOp::Min, Type::F64) => Value::F64(f64::INFINITY),
                (AggregateOp::Max, Type::F64) => Value::F64(f64::NEG_INFINITY),
                (AggregateOp::Min, _) => Value::I64(i64::MAX),
                _ => Value::I64(i64::MIN),
            });
            if !naive.same_as(g.value(id)) {
                return Err(format!(
                    "{} is {}, but over its members it is {naive}",
                    g.name(id),
                    g.value(id)
                ));
            }
        }

        // Rule 5: a node is recalculated only after everything it uses.
        let position: std::collections::HashMap<NodeId, usize> = recalculated
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();
        for &id in &recalculated {
            for dep in &g.dependencies(id) {
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
        parallel in any::<bool>(),
    ) {
        run(seed, inputs, calculated, updates, false, parallel).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn nonsense_expressions_are_refused_or_behave_correctly(
        seed in any::<u64>(),
        inputs in 1usize..8,
        calculated in 0usize..40,
        updates in 1usize..10,
        parallel in any::<bool>(),
    ) {
        run(seed, inputs, calculated, updates, true, parallel).map_err(TestCaseError::fail)?;
    }
}

/// A few bigger graphs, so the properties are also checked at a size where
/// long chains and wide fan-out appear.
#[test]
fn larger_random_graphs_always_match_a_full_recalculation() {
    for seed in 0..20 {
        run(seed, 30, 2000, 50, false, seed % 2 == 1)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
    }
}

/// Random inserts, updates and removals on a collection, checked after every
/// update against a plain map added up from scratch.
fn run_collection(seed: u64, keys: usize, steps: usize, parallel: bool) -> Result<(), String> {
    use std::collections::BTreeMap;
    use watershed_core::Collection;
    let mut rng = Rng(seed);
    let mut g = Graph::new();
    if parallel {
        g.set_parallel_threshold(1);
    }
    let bonus = g.input("bonus", 1_i64).map_err(|e| e.to_string())?;
    let mut c = Collection::new(&g, "rows");
    let x = c.field::<i64>("x").map_err(|e| e.to_string())?;
    let y = c.field::<i64>("y").map_err(|e| e.to_string())?;
    let score = c
        .column(&mut g, "score", x * 2 + y + bonus)
        .map_err(|e| e.to_string())?;
    let total = c.sum(&mut g, "total", score).map_err(|e| e.to_string())?;
    let count = c.count(&mut g, "count").map_err(|e| e.to_string())?;
    let lowest = c.min(&mut g, "lowest", x).map_err(|e| e.to_string())?;
    let highest = c.max(&mut g, "highest", score).map_err(|e| e.to_string())?;
    let mut model: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut bonus_value = 1_i64;
    for step in 0..steps {
        for _ in 0..1 + rng.below(4) {
            let key = format!("k{}", rng.below(keys));
            let (vx, vy) = (rng.below(21) as i64 - 10, rng.below(21) as i64 - 10);
            match rng.below(4) {
                0 => {
                    c.remove(&mut g, &key).map_err(|e| e.to_string())?;
                    model.remove(&key);
                }
                1 if model.contains_key(&key) => {
                    c.upsert(&mut g, &key, &[y.is(vy)])
                        .map_err(|e| e.to_string())?;
                    model.get_mut(&key).unwrap().1 = vy;
                }
                _ => {
                    c.upsert(&mut g, &key, &[x.is(vx), y.is(vy)])
                        .map_err(|e| e.to_string())?;
                    model.insert(key, (vx, vy));
                }
            }
        }
        if rng.percent(15) {
            bonus_value = rng.below(5) as i64;
            g.set(bonus, bonus_value).map_err(|e| e.to_string())?;
        }
        g.stabilize();
        g.check_consistency()?;
        let scores: Vec<i64> = model
            .values()
            .map(|&(x, y)| x * 2 + y + bonus_value)
            .collect();
        let expected = (
            scores.iter().sum::<i64>(),
            model.len() as i64,
            model.values().map(|v| v.0).min().unwrap_or(i64::MAX),
            scores.iter().copied().max().unwrap_or(i64::MIN),
        );
        let got = (
            g.get(total).map_err(|e| e.to_string())?,
            g.get(count).map_err(|e| e.to_string())?,
            g.get(lowest).map_err(|e| e.to_string())?,
            g.get(highest).map_err(|e| e.to_string())?,
        );
        if got != expected {
            return Err(format!(
                "step {step}: totals {got:?}, but adding up the rows gives {expected:?}"
            ));
        }
        if c.len() != model.len() {
            return Err(format!(
                "step {step}: {} rows, expected {}",
                c.len(),
                model.len()
            ));
        }
        for (key, &(vx, vy)) in &model {
            let s = c.get(&g, key, score).map_err(|e| e.to_string())?;
            if s != vx * 2 + vy + bonus_value {
                return Err(format!("step {step}: row {key} has score {s}"));
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn collections_match_a_plain_map_added_up_from_scratch(
        seed in any::<u64>(),
        keys in 1usize..40,
        steps in 1usize..30,
        parallel in any::<bool>(),
    ) {
        run_collection(seed, keys, steps, parallel).map_err(TestCaseError::fail)?;
    }
}
