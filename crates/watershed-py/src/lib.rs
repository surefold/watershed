//! Python bindings for watershed.
//!
//! ```python
//! import watershed as ws
//!
//! g = ws.Graph()
//! qty = g.input("qty", 10.0)
//! price = g.input("price", 2.5)
//! total = g.define("total", qty * price * 1.2)
//!
//! g.set(price, 3.0)
//! g.stabilize()          # recalculates total, nothing else
//! total.value            # 36.0
//! print(g.explain(total))
//! ```
//!
//! Python operators build expressions, exactly as the Rust API does: nothing
//! is calculated until an expression is defined as a node. Types are checked
//! as expressions are built, so a mistake such as adding a number to a string
//! raises straight away. A whole number next to a float is treated as a float.

use std::sync::Arc;

use pyo3::basic::CompareOp;
use pyo3::exceptions::{PyException, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyType};
use watershed_core::{
    AggregateOp, BinOp, CalcError as CoreError, Collection as CoreCollection, Graph as CoreGraph,
    Ir, NodeId, Type, UnOp, Value,
};

pyo3::create_exception!(
    watershed,
    CalcError,
    PyException,
    "A calculation could not be done: a node holds an error (such as a division by zero), or a request does not fit the graph."
);

fn to_py_err(e: CoreError) -> PyErr {
    match e {
        CoreError::Type(t) => PyTypeError::new_err(t.to_string()),
        CoreError::TypeMismatch { .. } => PyTypeError::new_err(e.to_string()),
        other => CalcError::new_err(other.to_string()),
    }
}

fn type_name(ty: Type) -> &'static str {
    match ty {
        Type::F64 => "float",
        Type::I64 => "int",
        Type::Bool => "bool",
        Type::Str => "str",
        Type::StrList => "list",
    }
}

/// A Python value as an engine value. `bool` is checked before `int`, since
/// in Python a bool is also an int.
fn to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if obj.is_instance_of::<PyBool>() {
        Ok(Value::Bool(obj.extract()?))
    } else if obj.is_instance_of::<PyInt>() {
        Ok(Value::I64(obj.extract().map_err(|_| {
            PyTypeError::new_err("whole numbers must fit in 64 bits")
        })?))
    } else if obj.is_instance_of::<PyFloat>() {
        Ok(Value::F64(obj.extract()?))
    } else if obj.is_instance_of::<PyString>() {
        Ok(Value::Str(obj.extract::<String>()?.into()))
    } else {
        Err(PyTypeError::new_err(format!(
            "watershed values are float, int, bool or str, not {}",
            obj.get_type().name()?
        )))
    }
}

/// An engine value as a Python value; an error value raises.
fn to_py(py: Python<'_>, v: &Value) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Value::F64(x) => x.into_pyobject(py)?.into_any().unbind(),
        Value::I64(x) => x.into_pyobject(py)?.into_any().unbind(),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any().unbind(),
        Value::Str(s) => PyString::new(py, s).into_any().unbind(),
        Value::List(items) => PyList::new(py, items.iter().map(|s| &**s))?
            .into_any()
            .unbind(),
        Value::Error(e) => return Err(CalcError::new_err(e.to_string())),
    })
}

/// A value for a node of type `ty`, from Python: a whole number given for a
/// float node is taken as a float.
fn value_for(ty: Type, obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    match (ty, to_value(obj)?) {
        (Type::F64, Value::I64(i)) => Ok(Value::F64(i as f64)),
        (_, v) => Ok(v),
    }
}

// ---------------------------------------------------------------- Expr

/// A node of a graph, or an expression built from nodes and values with
/// Python operators.
///
/// Arithmetic: `+ - * /`, unary `-`, `abs()`, `.min(x)`, `.max(x)`.
/// Comparisons: `< <= > >= == !=` (they build expressions too).
/// Logic on bools: `&`, `|`, `~`. Conditions: `when(c).then(a).otherwise(b)`.
#[pyclass(name = "Expr", module = "watershed", frozen)]
struct Expr {
    ir: Arc<Ir>,
    ty: Type,
    /// Set when this is a whole-number literal, so it can stand in for a float.
    literal: Option<i64>,
    /// Set when this is a node of a graph.
    node: Option<(Py<Graph>, NodeId)>,
}

impl Expr {
    fn new(ir: Ir, ty: Type) -> Expr {
        Expr {
            ir: Arc::new(ir),
            ty,
            literal: None,
            node: None,
        }
    }

    fn copy(&self, py: Python<'_>) -> Expr {
        Expr {
            ir: self.ir.clone(),
            ty: self.ty,
            literal: self.literal,
            node: self.node.as_ref().map(|(g, id)| (g.clone_ref(py), *id)),
        }
    }

    /// Any Python value or expression, as an expression.
    fn from_any(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Expr> {
        if let Ok(e) = obj.cast::<Expr>() {
            return Ok(e.get().copy(py));
        }
        let value = to_value(obj)?;
        let ty = value.type_of().expect("a Python value is never an error");
        let literal = match value {
            Value::I64(i) => Some(i),
            _ => None,
        };
        let mut e = Expr::new(Ir::Const(value), ty);
        e.literal = literal;
        Ok(e)
    }

    /// A whole-number literal next to a float becomes a float.
    fn unify(mut a: Expr, mut b: Expr) -> (Expr, Expr) {
        fn float(e: &mut Expr) {
            if let Some(i) = e.literal.take() {
                *e = Expr::new(Ir::Const(Value::F64(i as f64)), Type::F64);
            }
        }
        if a.ty == Type::F64 && b.ty == Type::I64 {
            float(&mut b);
        } else if b.ty == Type::F64 && a.ty == Type::I64 {
            float(&mut a);
        }
        (a, b)
    }

    fn arithmetic(op: BinOp, a: Expr, b: Expr) -> PyResult<Expr> {
        let (a, b) = Expr::unify(a, b);
        if a.ty != b.ty || !a.ty.is_numeric() {
            return Err(PyTypeError::new_err(format!(
                "{} needs two numbers of the same kind, got {} and {}",
                symbol(op),
                type_name(a.ty),
                type_name(b.ty)
            )));
        }
        Ok(Expr::new(Ir::Binary(op, a.ir, b.ir), a.ty))
    }

    fn compare(op: BinOp, a: Expr, b: Expr) -> PyResult<Expr> {
        let (a, b) = Expr::unify(a, b);
        let ordering = !matches!(op, BinOp::Eq | BinOp::Ne);
        if a.ty != b.ty || (ordering && !a.ty.is_ordered()) {
            return Err(PyTypeError::new_err(format!(
                "cannot compare {} with {} using {}",
                type_name(a.ty),
                type_name(b.ty),
                symbol(op)
            )));
        }
        Ok(Expr::new(Ir::Binary(op, a.ir, b.ir), Type::Bool))
    }

    fn logic(op: BinOp, a: Expr, b: Expr) -> PyResult<Expr> {
        if a.ty != Type::Bool || b.ty != Type::Bool {
            return Err(PyTypeError::new_err(format!(
                "{} needs two bools, got {} and {}",
                symbol(op),
                type_name(a.ty),
                type_name(b.ty)
            )));
        }
        Ok(Expr::new(Ir::Binary(op, a.ir, b.ir), Type::Bool))
    }

    fn unary(op: UnOp, a: Expr) -> PyResult<Expr> {
        let ok = match op {
            UnOp::Not => a.ty == Type::Bool,
            UnOp::Neg | UnOp::Abs => a.ty.is_numeric(),
            // Python has no way to ask for this yet.
            UnOp::ToText => false,
        };
        if !ok {
            return Err(PyTypeError::new_err(format!(
                "cannot apply {op:?} to {}",
                type_name(a.ty)
            )));
        }
        Ok(Expr::new(Ir::Unary(op, a.ir), a.ty))
    }

    fn node_id(&self) -> PyResult<NodeId> {
        self.node.as_ref().map(|(_, id)| *id).ok_or_else(|| {
            PyTypeError::new_err("this is an expression, not a node: define it first")
        })
    }
}

fn symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Min => "min",
        BinOp::Max => "max",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::And => "&",
        BinOp::Or => "|",
        BinOp::Concat => "+",
        BinOp::Join => "join",
    }
}

#[pymethods]
impl Expr {
    fn __add__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Add, self.copy(py), Expr::from_any(py, other)?)
    }
    fn __radd__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Add, Expr::from_any(py, other)?, self.copy(py))
    }
    fn __sub__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Sub, self.copy(py), Expr::from_any(py, other)?)
    }
    fn __rsub__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Sub, Expr::from_any(py, other)?, self.copy(py))
    }
    fn __mul__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Mul, self.copy(py), Expr::from_any(py, other)?)
    }
    fn __rmul__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Mul, Expr::from_any(py, other)?, self.copy(py))
    }
    fn __truediv__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Div, self.copy(py), Expr::from_any(py, other)?)
    }
    fn __rtruediv__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Div, Expr::from_any(py, other)?, self.copy(py))
    }
    fn __neg__(&self, py: Python<'_>) -> PyResult<Expr> {
        Expr::unary(UnOp::Neg, self.copy(py))
    }
    fn __abs__(&self, py: Python<'_>) -> PyResult<Expr> {
        Expr::unary(UnOp::Abs, self.copy(py))
    }
    fn __invert__(&self, py: Python<'_>) -> PyResult<Expr> {
        Expr::unary(UnOp::Not, self.copy(py))
    }
    fn __and__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::logic(BinOp::And, self.copy(py), Expr::from_any(py, other)?)
    }
    fn __rand__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::logic(BinOp::And, Expr::from_any(py, other)?, self.copy(py))
    }
    fn __or__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::logic(BinOp::Or, self.copy(py), Expr::from_any(py, other)?)
    }
    fn __ror__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::logic(BinOp::Or, Expr::from_any(py, other)?, self.copy(py))
    }
    fn __richcmp__(
        &self,
        py: Python<'_>,
        other: &Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<Expr> {
        let op = match op {
            CompareOp::Lt => BinOp::Lt,
            CompareOp::Le => BinOp::Le,
            CompareOp::Gt => BinOp::Gt,
            CompareOp::Ge => BinOp::Ge,
            CompareOp::Eq => BinOp::Eq,
            CompareOp::Ne => BinOp::Ne,
        };
        Expr::compare(op, self.copy(py), Expr::from_any(py, other)?)
    }

    /// The smaller of this and `other`.
    fn min(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Min, self.copy(py), Expr::from_any(py, other)?)
    }

    /// The larger of this and `other`.
    fn max(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<Expr> {
        Expr::arithmetic(BinOp::Max, self.copy(py), Expr::from_any(py, other)?)
    }

    fn __bool__(&self) -> PyResult<bool> {
        Err(PyTypeError::new_err(
            "an expression has no truth value until it is calculated: use when(...) for a condition, and .value to read a node",
        ))
    }

    fn __hash__(&self) -> PyResult<isize> {
        Err(PyTypeError::new_err(
            "expressions are not hashable, since == builds an expression: use node.name or node.id as a key",
        ))
    }

    /// The type of the value: "float", "int", "bool" or "str".
    #[getter]
    fn r#type(&self) -> &'static str {
        type_name(self.ty)
    }

    /// The node's id in its graph (nodes only).
    #[getter]
    fn id(&self) -> PyResult<u32> {
        Ok(self.node_id()?.index() as u32)
    }

    /// The node's name (nodes only).
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<String> {
        let (graph, id) = self
            .node
            .as_ref()
            .ok_or_else(|| PyTypeError::new_err("this is an expression, not a node"))?;
        Ok(graph.borrow(py).inner.name(*id).to_string())
    }

    /// The node's value as of the last update. Raises `CalcError` if it is an error.
    #[getter]
    fn value(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (graph, id) = self.node.as_ref().ok_or_else(|| {
            PyTypeError::new_err("this is an expression, not a node: define it first")
        })?;
        let graph = graph.borrow(py);
        if graph.inner.is_removed(*id) {
            return Err(CalcError::new_err("that node has been removed"));
        }
        to_py(py, graph.inner.value(*id))
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match &self.node {
            Some((graph, id)) => {
                let g = graph.borrow(py);
                format!(
                    "<watershed node {}: {} = {}>",
                    g.inner.name(*id),
                    type_name(self.ty),
                    g.inner.value(*id)
                )
            }
            None => format!("<watershed expression of {}>", type_name(self.ty)),
        }
    }
}

// ---------------------------------------------------------------- conditions

/// Starts a condition: `when(cond).then(a).otherwise(b)`. Only the branch
/// taken is evaluated, and the node only waits on what it read.
#[pyfunction]
fn when(py: Python<'_>, condition: &Bound<'_, PyAny>) -> PyResult<When> {
    let condition = Expr::from_any(py, condition)?;
    if condition.ty != Type::Bool {
        return Err(PyTypeError::new_err(format!(
            "a condition must be a bool, got {}",
            type_name(condition.ty)
        )));
    }
    Ok(When { condition })
}

#[pyclass(module = "watershed", frozen)]
struct When {
    condition: Expr,
}

#[pymethods]
impl When {
    fn then(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Then> {
        Ok(Then {
            condition: self.condition.copy(py),
            then: Expr::from_any(py, value)?,
        })
    }
}

#[pyclass(module = "watershed", frozen)]
struct Then {
    condition: Expr,
    then: Expr,
}

#[pymethods]
impl Then {
    fn otherwise(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Expr> {
        let (then, other) = Expr::unify(self.then.copy(py), Expr::from_any(py, value)?);
        if then.ty != other.ty {
            return Err(PyTypeError::new_err(format!(
                "both branches must have the same type, got {} and {}",
                type_name(then.ty),
                type_name(other.ty)
            )));
        }
        Ok(Expr::new(
            Ir::If(self.condition.ir.clone(), then.ir, other.ir),
            then.ty,
        ))
    }
}

/// The option at position `index` (counting from 0), like a spreadsheet's
/// CHOOSE. Only that option is evaluated; an index out of range is an error.
#[pyfunction]
fn choose(
    py: Python<'_>,
    index: &Bound<'_, PyAny>,
    options: Vec<Bound<'_, PyAny>>,
) -> PyResult<Expr> {
    let index = Expr::from_any(py, index)?;
    if index.ty != Type::I64 {
        return Err(PyTypeError::new_err(format!(
            "a choice's index must be an int, got {}",
            type_name(index.ty)
        )));
    }
    let mut options: Vec<Expr> = options
        .iter()
        .map(|o| Expr::from_any(py, o))
        .collect::<PyResult<_>>()?;
    if options.is_empty() {
        return Err(PyTypeError::new_err("choose needs at least one option"));
    }
    // Whole-number literals become floats if any option is a float.
    if options.iter().any(|o| o.ty == Type::F64) {
        for o in &mut options {
            if let Some(i) = o.literal.take() {
                *o = Expr::new(Ir::Const(Value::F64(i as f64)), Type::F64);
            }
        }
    }
    let ty = options[0].ty;
    if let Some(other) = options.iter().find(|o| o.ty != ty) {
        return Err(PyTypeError::new_err(format!(
            "every option must have the same type, got {} and {}",
            type_name(ty),
            type_name(other.ty)
        )));
    }
    Ok(Expr::new(
        Ir::Choose(index.ir, options.into_iter().map(|o| o.ir).collect()),
        ty,
    ))
}

// ---------------------------------------------------------------- Graph

/// A graph of inputs and calculated nodes.
#[pyclass(module = "watershed")]
struct Graph {
    inner: CoreGraph,
}

/// What one call to `Graph.stabilize()` did.
#[pyclass(module = "watershed", frozen, get_all)]
struct Report {
    /// This update's number; the first is 1.
    epoch: u64,
    /// Inputs whose value actually changed.
    inputs_changed: usize,
    /// Calculated nodes that were recalculated.
    recalculated: usize,
    /// Of those, how many came out with a different value.
    changed: usize,
}

#[pymethods]
impl Report {
    fn __repr__(&self) -> String {
        format!(
            "Report(epoch={}, inputs_changed={}, recalculated={}, changed={})",
            self.epoch, self.inputs_changed, self.recalculated, self.changed
        )
    }
}

impl Graph {
    fn node(slf: &Bound<'_, Self>, id: NodeId) -> Expr {
        let g = slf.borrow();
        Expr {
            ir: Arc::new(g.inner.reference(id)),
            ty: g.inner.node_type(id),
            literal: None,
            node: Some((slf.clone().unbind(), id)),
        }
    }

    /// A node of this graph, or an error if it belongs to another.
    fn own(slf: &Bound<'_, Self>, node: &Expr) -> PyResult<NodeId> {
        match &node.node {
            Some((graph, id)) if graph.is(slf) => {
                if slf.borrow().inner.is_removed(*id) {
                    return Err(CalcError::new_err("that node has been removed"));
                }
                Ok(*id)
            }
            Some(_) => Err(CalcError::new_err("that node belongs to a different graph")),
            None => Err(PyTypeError::new_err(
                "this is an expression, not a node: define it first",
            )),
        }
    }
}

#[pymethods]
impl Graph {
    #[new]
    fn new() -> Self {
        Graph {
            inner: CoreGraph::new(),
        }
    }

    /// Adds an input with a starting value (float, int, bool or str).
    fn input(slf: &Bound<'_, Self>, name: &str, value: &Bound<'_, PyAny>) -> PyResult<Expr> {
        let value = to_value(value)?;
        let id = slf
            .borrow_mut()
            .inner
            .input_value(name, value)
            .map_err(to_py_err)?;
        Ok(Graph::node(slf, id))
    }

    /// Adds a calculated node from an expression (or a constant).
    fn define(slf: &Bound<'_, Self>, name: &str, expr: &Bound<'_, PyAny>) -> PyResult<Expr> {
        let expr = Expr::from_any(slf.py(), expr)?;
        let id = slf
            .borrow_mut()
            .inner
            .define_ir(name, (*expr.ir).clone())
            .map_err(to_py_err)?;
        Ok(Graph::node(slf, id))
    }

    /// A formula written as text, such as `"qty * price * (1 + tax)"`, as an
    /// expression. Each name is the node of that name; a name with spaces or
    /// punctuation goes between backticks. Use it wherever an expression goes:
    /// `g.define("total", g.formula("qty * price"))`.
    fn formula(slf: &Bound<'_, Self>, source: &str) -> PyResult<Expr> {
        let (ir, ty) = slf
            .borrow()
            .inner
            .parse_formula(source)
            .map_err(to_py_err)?;
        Ok(Expr::new(ir, ty))
    }

    /// Removes a node that nothing uses. It is no longer calculated or
    /// found, its name is free for a new node, and using it is an error.
    fn remove(slf: &Bound<'_, Self>, node: &Bound<'_, Expr>) -> PyResult<()> {
        let id = Graph::own(slf, node.get())?;
        slf.borrow_mut().inner.remove_node(id).map_err(to_py_err)
    }

    /// Records a new value for an input; it takes effect at the next `stabilize()`.
    fn set(
        slf: &Bound<'_, Self>,
        node: &Bound<'_, Expr>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let id = Graph::own(slf, node.get())?;
        let mut g = slf.borrow_mut();
        let value = value_for(g.inner.node_type(id), value)?;
        g.inner.set_value(id, value).map_err(to_py_err)
    }

    /// Applies the recorded changes, recalculating only what depends on them.
    fn stabilize(&mut self, py: Python<'_>) -> Report {
        // The engine does not touch Python objects, so others can run meanwhile.
        let inner = &mut self.inner;
        let r = py.detach(|| inner.stabilize());
        Report {
            epoch: r.epoch,
            inputs_changed: r.inputs_changed,
            recalculated: r.recalculated,
            changed: r.changed,
        }
    }

    /// A node's value as of the last update. Raises `CalcError` if it is an error.
    fn get(slf: &Bound<'_, Self>, node: &Bound<'_, Expr>) -> PyResult<Py<PyAny>> {
        let id = Graph::own(slf, node.get())?;
        to_py(slf.py(), slf.borrow().inner.value(id))
    }

    /// Why a node has its value: its formula and what it read, as a tree.
    fn explain(slf: &Bound<'_, Self>, node: &Bound<'_, Expr>) -> PyResult<String> {
        let id = Graph::own(slf, node.get())?;
        Ok(slf.borrow().inner.explain_node(id))
    }

    /// The whole graph, level by level.
    fn describe(&self) -> String {
        self.inner.describe()
    }

    /// The graph in Graphviz's DOT language.
    fn to_dot(&self) -> String {
        self.inner.to_dot()
    }

    /// The names of the nodes the last update recalculated, in order.
    fn last_recalculated(&self) -> Vec<String> {
        self.inner
            .last_recalculated()
            .iter()
            .map(|&id| self.inner.name(id).to_string())
            .collect()
    }

    /// The node with this name, or None.
    fn find(slf: &Bound<'_, Self>, name: &str) -> Option<Expr> {
        let id = slf.borrow().inner.find(name)?;
        Some(Graph::node(slf, id))
    }

    /// Recalculates every node from scratch and compares with the
    /// incremental values; raises `CalcError` if any differ.
    fn check_consistency(&self) -> PyResult<()> {
        self.inner.check_consistency().map_err(CalcError::new_err)
    }

    /// Levels with at least this many nodes to recalculate use all CPU cores.
    fn set_parallel_threshold(&mut self, nodes: usize) {
        self.inner.set_parallel_threshold(nodes);
    }

    /// A new, empty collection (a keyed table of rows) in this graph.
    fn collection(slf: &Bound<'_, Self>, name: &str) -> Collection {
        let inner = CoreCollection::new(&slf.borrow().inner, name);
        Collection {
            graph: slf.clone().unbind(),
            inner,
            field_types: Vec::new(),
        }
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "<watershed graph of {} nodes, after update {}>",
            self.inner.len(),
            self.inner.epoch()
        )
    }
}

// ---------------------------------------------------------------- Collection

/// A keyed table of rows, with typed fields, calculated columns and totals
/// that update by only the rows that change.
///
/// ```python
/// orders = g.collection("orders")
/// qty = orders.field("qty", float)
/// price = orders.field("price", float)
/// line = orders.column("line_total", qty * price)
/// revenue = orders.sum("revenue", line)
/// orders.upsert("o-1", qty=2, price=9.99)
/// g.stabilize()
/// ```
#[pyclass(module = "watershed")]
struct Collection {
    graph: Py<Graph>,
    inner: CoreCollection,
    field_types: Vec<(String, Type)>,
}

impl Collection {
    fn grouping(
        slf: &Bound<'_, Self>,
        name: &str,
        op: AggregateOp,
        by: &Bound<'_, Expr>,
        of: Option<&Bound<'_, Expr>>,
    ) -> PyResult<Groups> {
        let py = slf.py();
        let mut this = slf.borrow_mut();
        let graph = this.graph.clone_ref(py);
        let by_type = by.get().ty;
        let index = this
            .inner
            .add_grouping(
                &mut graph.bind(py).borrow_mut().inner,
                name,
                op,
                &by.get().ir,
                of.map(|o| &*o.get().ir),
            )
            .map_err(to_py_err)?;
        Ok(Groups {
            collection: slf.clone().unbind(),
            index,
            key_type: by_type,
        })
    }

    fn total(
        &mut self,
        py: Python<'_>,
        name: &str,
        op: AggregateOp,
        of: Option<&Expr>,
    ) -> PyResult<Expr> {
        let graph = self.graph.bind(py);
        let id = self
            .inner
            .add_total(&mut graph.borrow_mut().inner, name, op, of.map(|e| &*e.ir))
            .map_err(to_py_err)?;
        Ok(Graph::node(graph, id))
    }
}

#[pymethods]
impl Collection {
    /// Adds a field of type float, int, bool or str. Fields come before the first row.
    fn field(&mut self, py: Python<'_>, name: &str, r#type: &Bound<'_, PyType>) -> PyResult<Expr> {
        let ty = if r#type.is(py.get_type::<PyBool>()) {
            Type::Bool
        } else if r#type.is(py.get_type::<PyInt>()) {
            Type::I64
        } else if r#type.is(py.get_type::<PyFloat>()) {
            Type::F64
        } else if r#type.is(py.get_type::<PyString>()) {
            Type::Str
        } else {
            return Err(PyTypeError::new_err(
                "a field's type is float, int, bool or str",
            ));
        };
        let ir = self.inner.add_field(name, ty).map_err(to_py_err)?;
        self.field_types.push((name.to_string(), ty));
        Ok(Expr::new(ir, ty))
    }

    /// Adds a column, worked out in every row by `formula`.
    fn column(&mut self, py: Python<'_>, name: &str, formula: &Bound<'_, PyAny>) -> PyResult<Expr> {
        let formula = Expr::from_any(py, formula)?;
        let graph = self.graph.bind(py);
        let (ir, ty) = self
            .inner
            .add_column(&mut graph.borrow_mut().inner, name, (*formula.ir).clone())
            .map_err(to_py_err)?;
        Ok(Expr::new(ir, ty))
    }

    /// A formula written as text, such as `"qty * price * fx"`, as an
    /// expression for `column`. A name is one of this collection's fields
    /// or columns, or else the graph's node of that name.
    fn formula(&self, py: Python<'_>, source: &str) -> PyResult<Expr> {
        let graph = self.graph.bind(py);
        let (ir, ty) = self
            .inner
            .parse_formula(&graph.borrow().inner, source)
            .map_err(to_py_err)?;
        Ok(Expr::new(ir, ty))
    }

    /// A total: the sum of a field or column over the present rows.
    fn sum(&mut self, py: Python<'_>, name: &str, of: &Bound<'_, Expr>) -> PyResult<Expr> {
        self.total(py, name, AggregateOp::Sum, Some(of.get()))
    }

    /// A total: the smallest value of a field or column over the present rows.
    fn min(&mut self, py: Python<'_>, name: &str, of: &Bound<'_, Expr>) -> PyResult<Expr> {
        self.total(py, name, AggregateOp::Min, Some(of.get()))
    }

    /// A total: the largest value of a field or column over the present rows.
    fn max(&mut self, py: Python<'_>, name: &str, of: &Bound<'_, Expr>) -> PyResult<Expr> {
        self.total(py, name, AggregateOp::Max, Some(of.get()))
    }

    /// A total: how many rows are present.
    fn count(&mut self, py: Python<'_>, name: &str) -> PyResult<Expr> {
        self.total(py, name, AggregateOp::Count, None)
    }

    /// Totals per group: the sum of `of` over the rows with each value of
    /// the field `by`. Returns a `Groups`, which works like a read-only dict
    /// from each key to its total's node.
    fn group_sum(
        slf: &Bound<'_, Self>,
        name: &str,
        by: &Bound<'_, Expr>,
        of: &Bound<'_, Expr>,
    ) -> PyResult<Groups> {
        Collection::grouping(slf, name, AggregateOp::Sum, by, Some(of))
    }

    /// Totals per group: the smallest value of `of` in each group.
    fn group_min(
        slf: &Bound<'_, Self>,
        name: &str,
        by: &Bound<'_, Expr>,
        of: &Bound<'_, Expr>,
    ) -> PyResult<Groups> {
        Collection::grouping(slf, name, AggregateOp::Min, by, Some(of))
    }

    /// Totals per group: the largest value of `of` in each group.
    fn group_max(
        slf: &Bound<'_, Self>,
        name: &str,
        by: &Bound<'_, Expr>,
        of: &Bound<'_, Expr>,
    ) -> PyResult<Groups> {
        Collection::grouping(slf, name, AggregateOp::Max, by, Some(of))
    }

    /// Totals per group: how many rows each group has.
    fn group_count(slf: &Bound<'_, Self>, name: &str, by: &Bound<'_, Expr>) -> PyResult<Groups> {
        Collection::grouping(slf, name, AggregateOp::Count, by, None)
    }

    /// Adds a row, or changes the given fields of an existing one:
    /// `upsert("o-1", qty=2, price=9.99)`. A new row needs every field.
    /// Takes effect at the next `stabilize()`.
    #[pyo3(signature = (key, **fields))]
    fn upsert(
        &mut self,
        py: Python<'_>,
        key: &str,
        fields: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let mut row: Vec<(String, Value)> = Vec::new();
        if let Some(fields) = fields {
            for (name, value) in fields.iter() {
                let name: String = name.extract()?;
                let ty = self
                    .field_types
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, t)| *t)
                    .ok_or_else(|| CalcError::new_err(format!("there is no field {name:?}")))?;
                row.push((name, value_for(ty, &value)?));
            }
        }
        let row: Vec<(&str, Value)> = row.iter().map(|(n, v)| (n.as_str(), v.clone())).collect();
        let graph = self.graph.bind(py);
        self.inner
            .upsert_named(&mut graph.borrow_mut().inner, key, &row)
            .map_err(to_py_err)
    }

    /// Removes a row; returns whether there was one. Takes effect at the next `stabilize()`.
    fn remove(&mut self, py: Python<'_>, key: &str) -> PyResult<bool> {
        let graph = self.graph.bind(py);
        self.inner
            .remove(&mut graph.borrow_mut().inner, key)
            .map_err(to_py_err)
    }

    /// A row's field or column, as of the last update.
    fn get(&self, py: Python<'_>, key: &str, of: &Bound<'_, Expr>) -> PyResult<Py<PyAny>> {
        let graph = self.graph.bind(py).borrow();
        let value = self
            .inner
            .get_value(&graph.inner, key, &of.get().ir)
            .map_err(to_py_err)?;
        to_py(py, &value)
    }

    /// The rows' keys.
    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.inner.keys().map(str::to_string).collect();
        keys.sort();
        keys
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __contains__(&self, key: &str) -> bool {
        self.inner.contains(key)
    }
}

/// A grouped total: one total per distinct value of a field. Works like a
/// read-only dict from each key to its total's node: `groups["EU"].value`,
/// `groups.keys()`, `"EU" in groups`, `groups.to_dict()`.
#[pyclass(module = "watershed")]
struct Groups {
    collection: Py<Collection>,
    index: usize,
    key_type: Type,
}

#[pymethods]
impl Groups {
    /// The total's node for one group, or None if no row has had that key.
    fn get(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Option<Expr>> {
        let key = value_for(self.key_type, key)?;
        let c = self.collection.borrow(py);
        let graph = c.graph.bind(py);
        let Some(id) = c.inner.group_node(&graph.borrow().inner, self.index, &key) else {
            return Ok(None);
        };
        Ok(Some(Graph::node(graph, id)))
    }

    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Expr> {
        self.get(py, key)?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(key.to_string()))
    }

    /// The keys of the groups that have rows, sorted.
    fn keys(&self, py: Python<'_>) -> Vec<String> {
        let c = self.collection.borrow(py);
        c.inner
            .group_keys_of(&c.graph.bind(py).borrow().inner, self.index)
    }

    fn __contains__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<bool> {
        let key = value_for(self.key_type, key)?;
        let text = match &key {
            Value::Str(s) => s.to_string(),
            other => other.to_string(),
        };
        Ok(self.keys(py).contains(&text))
    }

    fn __len__(&self, py: Python<'_>) -> usize {
        self.keys(py).len()
    }

    /// Every group that has rows, with its total as of the last update.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let dict = PyDict::new(py);
        let c = self.collection.borrow(py);
        let graph = c.graph.bind(py).borrow();
        for key in c.inner.group_keys_of(&graph.inner, self.index) {
            let value = match self.key_type {
                Type::Str => Value::Str(key.clone().into()),
                Type::I64 => Value::I64(key.parse().unwrap_or_default()),
                Type::Bool => Value::Bool(key == "true"),
                Type::F64 => Value::F64(key.parse().unwrap_or_default()),
                // A Python field cannot be a list, so no group has this key.
                Type::StrList => continue,
            };
            if let Some(id) = c.inner.group_node(&graph.inner, self.index, &value) {
                dict.set_item(to_py(py, &value)?, to_py(py, graph.inner.value(id))?)?;
            }
        }
        Ok(dict.unbind())
    }
}

// ---------------------------------------------------------------- module

/// watershed: an incremental calculation engine. Change an input, and only
/// what depends on it is recalculated.
#[pymodule]
fn watershed(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Graph>()?;
    m.add_class::<Expr>()?;
    m.add_class::<Collection>()?;
    m.add_class::<Report>()?;
    m.add_class::<Groups>()?;
    m.add_class::<When>()?;
    m.add_class::<Then>()?;
    m.add_function(wrap_pyfunction!(when, m)?)?;
    m.add_function(wrap_pyfunction!(choose, m)?)?;
    m.add("CalcError", m.py().get_type::<CalcError>())?;
    Ok(())
}
