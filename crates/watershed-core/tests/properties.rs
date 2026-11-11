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
use watershed_core::{AggregateOp, BinOp, CalcError, Graph, Ir, NodeId, Text, Type, UnOp, Value};

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

const TYPES: [Type; 5] = [Type::F64, Type::I64, Type::Bool, Type::Str, Type::StrList];

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
        Type::StrList => {
            let items = (0..rng.below(3))
                .map(|_| Text::from(rng.pick(&["", "a", "b"])))
                .collect::<Vec<_>>();
            Value::List(items.into())
        }
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
                if rng.percent(25) {
                    let op = rng.pick(&[BinOp::Eq, BinOp::Ne]);
                    return Ir::Binary(op, sub(rng, Type::StrList), sub(rng, Type::StrList));
                }
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
        Type::StrList => match rng.below(3) {
            0 => Ir::Binary(
                BinOp::Concat,
                sub(rng, Type::StrList),
                sub(rng, Type::StrList),
            ),
            1 => Ir::List((0..rng.below(3)).map(|_| sub(rng, Type::Str)).collect()),
            _ => Ir::If(
                sub(rng, Type::Bool),
                sub(rng, Type::StrList),
                sub(rng, Type::StrList),
            ),
        },
        Type::Str => match rng.below(4) {
            0 => Ir::Binary(BinOp::Concat, sub(rng, Type::Str), sub(rng, Type::Str)),
            2 => Ir::Binary(BinOp::Join, sub(rng, Type::StrList), sub(rng, Type::Str)),
            1 => {
                let operand = rng.pick(&TYPES);
                Ir::Unary(UnOp::ToText, sub(rng, operand))
            }
            _ => Ir::If(
                sub(rng, Type::Bool),
                sub(rng, Type::Str),
                sub(rng, Type::Str),
            ),
        },
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
    match rng.below(5) {
        4 => Ir::List((0..rng.below(3)).map(|_| sub(rng)).collect()),
        3 => {
            let options = (0..rng.below(3)).map(|_| sub(rng)).collect();
            Ir::Choose(sub(rng), options)
        }
        0 => Ir::Unary(
            rng.pick(&[UnOp::Neg, UnOp::Abs, UnOp::Not, UnOp::ToText]),
            sub(rng),
        ),
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
                BinOp::Concat,
                BinOp::Join,
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

/// Whether `ir` uses `target`, directly or through the nodes it uses
/// (counting `staged` members, which have not joined their aggregates yet).
/// A plain search, independent of the engine's own (which prunes by height).
fn reaches(g: &Graph, ir: &Ir, target: NodeId, staged: &[(NodeId, NodeId, NodeId)]) -> bool {
    let mut refs = Vec::new();
    ir.refs(&mut refs);
    let mut stack: Vec<NodeId> = refs.iter().map(|r| r.node).collect();
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if n == target {
            return true;
        }
        if seen.insert(n) {
            stack.extend(g.dependencies(n));
            for &(aggregate, value, present) in staged {
                if aggregate == n {
                    stack.extend([value, present]);
                }
            }
        }
    }
    false
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
        let mut staged: Vec<(NodeId, NodeId, NodeId)> = Vec::new();
        if !aggregates.is_empty() && rng.percent(40) {
            let (aggregate, op, value_type) = rng.pick(&aggregates);
            if let Some((value, present)) = random_member(&mut rng, &g, op, value_type) {
                match g.extend_aggregate(aggregate, value, present) {
                    Ok(()) => {
                        joined.insert(aggregate);
                        staged.push((aggregate, value, present));
                    }
                    Err(CalcError::Invalid(_)) => {}
                    Err(other) => return Err(format!("extending an aggregate failed: {other}")),
                }
            }
        }

        // Sometimes a calculated node gets a new expression, possibly using
        // nodes above it. It must be refused exactly when that would make the
        // node depend on itself, and if accepted it counts from this update.
        let mut redefined: HashSet<NodeId> = HashSet::new();
        for _ in 0..rng.below(3) {
            if rng.percent(50) {
                continue;
            }
            let calculated: Vec<NodeId> = g
                .node_ids()
                .filter(|&id| !g.is_input(id) && g.aggregate_members(id).is_none())
                .collect();
            if calculated.is_empty() {
                break;
            }
            let id = rng.pick(&calculated);
            let ir = if nonsense {
                random_untyped_expr(&mut rng, &g, 3)
            } else {
                random_expr(&mut rng, &g, g.node_type(id), 3)
            };
            let cycle = reaches(&g, &ir, id, &staged);
            match g.redefine_ir(id, ir.clone()) {
                Ok(()) => {
                    if cycle {
                        return Err(format!("{} was allowed to use itself", g.name(id)));
                    }
                    let rebuilt = g.expression(id).expect("a calculated node");
                    if !rebuilt.same_as(&ir) {
                        return Err("redefining changed the expression".into());
                    }
                    redefined.insert(id);
                }
                Err(CalcError::Invalid(message)) if message.contains("depend on itself") => {
                    if !cycle {
                        return Err(format!("refused without a cycle: {message}"));
                    }
                }
                Err(CalcError::Type(_) | CalcError::TypeMismatch { .. }) if nonsense => {}
                Err(other) => return Err(format!("redefining failed: {other}")),
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
            // So is a node given a new expression.
            let should = read_a_change || joined.contains(&id) || redefined.contains(&id);
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

/// Random updates with history on. After every update, every node at every
/// update is compared with a full copy of the values taken at that update.
fn run_history(
    seed: u64,
    keep: Option<u64>,
    keep_from_start: bool,
    updates: usize,
    parallel: bool,
) -> Result<(), String> {
    let keep = keep.unwrap_or(u64::MAX);
    let snapshot =
        |g: &Graph| -> Vec<Value> { g.node_ids().map(|id| g.value(id).clone()).collect() };
    let mut rng = Rng(seed);
    let mut g = Graph::new();
    if parallel {
        g.set_parallel_threshold(1);
    }
    if keep_from_start {
        g.keep_history(keep);
    }
    let mut input_ids = Vec::new();
    for i in 0..1 + rng.below(7) {
        let ty = rng.pick(&TYPES);
        input_ids.push(
            g.input_value(&format!("in{i}"), random_value(&mut rng, ty))
                .unwrap(),
        );
    }
    let mut n = 0;
    for _ in 0..rng.below(40) {
        add_random_node(&mut rng, &mut g, n, false);
        n += 1;
    }
    let aggregates: Vec<(NodeId, AggregateOp, Type)> = (0..rng.below(4))
        .map(|k| add_random_aggregate(&mut rng, &mut g, k))
        .collect();
    if !keep_from_start {
        g.keep_history(keep);
    }

    // What each update should have left: all values, and how many were overwritten.
    // A graph with history on from the start had no nodes at update 0.
    let mut snapshots: Vec<Vec<Value>> = vec![if keep_from_start {
        Vec::new()
    } else {
        snapshot(&g)
    }];
    let mut recorded: Vec<usize> = vec![0];
    // How every node explained itself after each update.
    let explained =
        |g: &Graph| -> Vec<String> { g.node_ids().map(|id| g.explain_node(id)).collect() };
    let mut explanations: Vec<Vec<String>> = vec![Vec::new()];

    for _ in 0..updates {
        for _ in 0..1 + rng.below(3) {
            let id = rng.pick(&input_ids);
            let value = if rng.percent(10) {
                Value::Error("feed down".into())
            } else {
                random_value(&mut rng, g.node_type(id))
            };
            g.set_value(id, value).unwrap();
        }
        if rng.percent(20) {
            add_random_node(&mut rng, &mut g, n, false);
            n += 1;
        }
        if !aggregates.is_empty() && rng.percent(40) {
            let (aggregate, op, value_type) = rng.pick(&aggregates);
            if let Some((value, present)) = random_member(&mut rng, &g, op, value_type) {
                let _ = g.extend_aggregate(aggregate, value, present);
            }
        }
        if rng.percent(20) {
            let calculated: Vec<NodeId> = g
                .node_ids()
                .filter(|&id| !g.is_input(id) && g.aggregate_members(id).is_none())
                .collect();
            if !calculated.is_empty() {
                let id = rng.pick(&calculated);
                let ir = random_expr(&mut rng, &g, g.node_type(id), 3);
                let _ = g.redefine_ir(id, ir);
            }
        }

        let report = g.stabilize();
        snapshots.push(snapshot(&g));
        recorded.push(report.inputs_changed + report.changed);
        explanations.push(explained(&g));

        let latest = report.epoch;
        let oldest = latest.saturating_sub(keep);
        if g.history_range() != Some((oldest, latest)) {
            return Err(format!("history covers {:?}", g.history_range()));
        }
        // Exactly the values overwritten by the updates after the oldest.
        let expected: usize = recorded[oldest as usize + 1..].iter().sum();
        if g.history_versions() != expected {
            return Err(format!(
                "{} versions kept at update {latest}, expected {expected}",
                g.history_versions()
            ));
        }
        for epoch in 0..=latest {
            let then = &snapshots[epoch as usize];
            for id in g.node_ids() {
                let found = g.value_at(id, epoch);
                if epoch < oldest {
                    if !matches!(found, Err(CalcError::EpochUnavailable { .. })) {
                        return Err(format!("update {epoch} should be forgotten: {found:?}"));
                    }
                } else if id.index() >= then.len() {
                    if !matches!(found, Err(CalcError::NotCreatedYet { .. })) {
                        return Err(format!(
                            "{} should not exist at update {epoch}: {found:?}",
                            g.name(id)
                        ));
                    }
                } else {
                    let value = found.map_err(|e| e.to_string())?;
                    if !value.same_as(&then[id.index()]) {
                        return Err(format!(
                            "{} at update {epoch} reads {value}, but was {}",
                            g.name(id),
                            then[id.index()]
                        ));
                    }
                    // Explained as of then, it says what it said then,
                    // unless something under it has a new expression since
                    // (which is not kept, and says so).
                    let said = &explanations[epoch as usize];
                    if epoch > 0 && id.index() < said.len() {
                        let now = g.explain_node_at(id, epoch).map_err(|e| e.to_string())?;
                        if now != said[id.index()] && !now.contains("(redefined at update ") {
                            return Err(format!(
                                "{} explained as of update {epoch}:\n{now}but at the time:\n{}",
                                g.name(id),
                                said[id.index()]
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn every_node_reads_as_it_was_at_every_update_history_covers(
        seed in any::<u64>(),
        keep in proptest::option::of(0u64..12),
        keep_from_start in any::<bool>(),
        updates in 1usize..25,
        parallel in any::<bool>(),
    ) {
        run_history(seed, keep, keep_from_start, updates, parallel)
            .map_err(TestCaseError::fail)?;
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
    let group = c.field::<String>("group").map_err(|e| e.to_string())?;
    let score = c
        .column(&mut g, "score", x * 2 + y + bonus)
        .map_err(|e| e.to_string())?;
    let total = c.sum(&mut g, "total", score).map_err(|e| e.to_string())?;
    let count = c.count(&mut g, "count").map_err(|e| e.to_string())?;
    let lowest = c.min(&mut g, "lowest", x).map_err(|e| e.to_string())?;
    let highest = c.max(&mut g, "highest", score).map_err(|e| e.to_string())?;
    let group_totals = c
        .group_sum(&mut g, "group_total", group, score)
        .map_err(|e| e.to_string())?;
    let group_counts = c
        .group_count(&mut g, "group_count", group)
        .map_err(|e| e.to_string())?;
    let mut model: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut groups: BTreeMap<String, String> = BTreeMap::new();
    let names = ["north", "south", "east", "west"];
    let mut bonus_value = 1_i64;
    for step in 0..steps {
        for _ in 0..1 + rng.below(4) {
            let key = format!("k{}", rng.below(keys));
            let (vx, vy) = (rng.below(21) as i64 - 10, rng.below(21) as i64 - 10);
            match rng.below(4) {
                0 => {
                    c.remove(&mut g, &key).map_err(|e| e.to_string())?;
                    model.remove(&key);
                    groups.remove(&key);
                }
                // Move an existing row to another group.
                3 if model.contains_key(&key) => {
                    let name = names[rng.below(names.len())].to_string();
                    c.upsert(&mut g, &key, &[group.is(name.clone())])
                        .map_err(|e| e.to_string())?;
                    groups.insert(key, name);
                }
                1 if model.contains_key(&key) => {
                    c.upsert(&mut g, &key, &[y.is(vy)])
                        .map_err(|e| e.to_string())?;
                    model.get_mut(&key).unwrap().1 = vy;
                }
                _ => {
                    let name = names[rng.below(names.len())].to_string();
                    c.upsert(&mut g, &key, &[x.is(vx), y.is(vy), group.is(name.clone())])
                        .map_err(|e| e.to_string())?;
                    model.insert(key.clone(), (vx, vy));
                    groups.insert(key, name);
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
        // Every group's total and count against the plain map.
        let mut expected_keys: Vec<String> = Vec::new();
        for name in names {
            let members: Vec<i64> = model
                .iter()
                .filter(|(k, _)| groups.get(*k).map(String::as_str) == Some(name))
                .map(|(_, &(x, y))| x * 2 + y + bonus_value)
                .collect();
            if !members.is_empty() {
                expected_keys.push(name.to_string());
            }
            let total = c.group(&g, group_totals, name.to_string());
            let count = c.group(&g, group_counts, name.to_string());
            match (total, count) {
                (Some(total), Some(count)) => {
                    let got = (
                        g.get(total).map_err(|e| e.to_string())?,
                        g.get(count).map_err(|e| e.to_string())?,
                    );
                    let want = (members.iter().sum::<i64>(), members.len() as i64);
                    if got != want {
                        return Err(format!(
                            "step {step}: group {name} has {got:?}, expected {want:?}"
                        ));
                    }
                }
                (None, None) if members.is_empty() => {}
                _ => return Err(format!("step {step}: group {name} is missing")),
            }
        }
        expected_keys.sort();
        if c.group_keys(group_totals) != expected_keys {
            return Err(format!(
                "step {step}: groups {:?}, expected {expected_keys:?}",
                c.group_keys(group_totals)
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

/// Random inserts, updates and removals on a collection with history on.
/// After every update, every key is read at every update history covers and
/// compared with a plain copy of the rows taken at that update. Removed rows'
/// slots are reused, so a key moves between slots and a slot between keys.
fn run_collection_history(
    seed: u64,
    keys: usize,
    steps: usize,
    keep: Option<u64>,
    warm_up: usize,
) -> Result<(), String> {
    use std::collections::BTreeMap;
    use watershed_core::Collection;
    let e = |e: CalcError| e.to_string();
    let keep = keep.unwrap_or(u64::MAX);
    let mut rng = Rng(seed);
    let mut g = Graph::new();
    let bonus = g.input("bonus", 1_i64).map_err(e)?;
    let mut c = Collection::new(&g, "rows");
    let x = c.field::<i64>("x").map_err(e)?;
    let y = c.field::<i64>("y").map_err(e)?;
    let score = c.column(&mut g, "score", x * 2 + y + bonus).map_err(e)?;
    let mut model: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut bonus_value = 1_i64;
    // What each update should have left: key -> (x, score).
    let mut snapshots: BTreeMap<u64, BTreeMap<String, (i64, i64)>> = BTreeMap::new();
    let all_keys: Vec<String> = (0..keys).map(|k| format!("k{k}")).collect();
    // The rows as of the last update, for when history starts.
    let mut last: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for step in 0..steps {
        let changes = 1 + rng.below(4);
        // History starts part way through a step's changes, so some changes
        // made while it was off count from its first update.
        let start_after = rng.below(changes + 1);
        for change in 0..changes {
            if step == warm_up && change == start_after {
                g.keep_history(keep);
                snapshots.insert(g.epoch(), last.clone());
            }
            let key = rng.pick(&all_keys);
            let (vx, vy) = (rng.below(21) as i64 - 10, rng.below(21) as i64 - 10);
            match rng.below(4) {
                0 => {
                    c.remove(&mut g, &key).map_err(e)?;
                    model.remove(&key);
                }
                1 if model.contains_key(&key) => {
                    c.upsert(&mut g, &key, &[y.is(vy)]).map_err(e)?;
                    model.get_mut(&key).unwrap().1 = vy;
                }
                _ => {
                    c.upsert(&mut g, &key, &[x.is(vx), y.is(vy)]).map_err(e)?;
                    model.insert(key, (vx, vy));
                }
            }
        }
        if step == warm_up && start_after == changes {
            g.keep_history(keep);
            snapshots.insert(g.epoch(), last.clone());
        }
        if rng.percent(15) {
            bonus_value = rng.below(5) as i64;
            g.set(bonus, bonus_value).map_err(e)?;
        }
        let report = g.stabilize();
        last = model
            .iter()
            .map(|(k, &(vx, vy))| (k.clone(), (vx, vx * 2 + vy + bonus_value)))
            .collect();
        if step < warm_up {
            continue;
        }
        snapshots.insert(report.epoch, last.clone());
        let (oldest, latest) = g.history_range().ok_or("history is off")?;
        for (&epoch, then) in &snapshots {
            if epoch < oldest {
                if !matches!(
                    c.keys_at(&g, epoch),
                    Err(CalcError::EpochUnavailable { .. })
                ) {
                    return Err(format!("update {epoch} should be forgotten"));
                }
                continue;
            }
            let found = c.keys_at(&g, epoch).map_err(e)?;
            let expected: Vec<String> = then.keys().cloned().collect();
            if found != expected {
                return Err(format!(
                    "at update {epoch} (latest {latest}): keys {found:?}, expected {expected:?}"
                ));
            }
            for key in &all_keys {
                let had = c.contains_at(&g, key, epoch).map_err(e)?;
                match then.get(key) {
                    Some(&(vx, vscore)) => {
                        let got = (
                            c.get_at(&g, key, x, epoch).map_err(e)?,
                            c.get_at(&g, key, score, epoch).map_err(e)?,
                        );
                        if !had || got != (vx, vscore) {
                            return Err(format!(
                                "row {key} at update {epoch}: {got:?} (present {had}), expected {:?}",
                                (vx, vscore)
                            ));
                        }
                    }
                    None => {
                        if had || !matches!(c.get_at(&g, key, x, epoch), Err(CalcError::Invalid(_)))
                        {
                            return Err(format!("row {key} should be absent at update {epoch}"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn a_collection_reads_as_it_was_at_every_update_history_covers(
        seed in any::<u64>(),
        keys in 1usize..12,
        steps in 1usize..25,
        keep in proptest::option::of(0u64..8),
        warm_up in 0usize..5,
    ) {
        run_collection_history(seed, keys, steps, keep, warm_up).map_err(TestCaseError::fail)?;
    }
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
