# watershed for Python

An incremental calculation engine, written in Rust: build a graph of inputs
and calculated nodes, change an input, and only what depends on it is
recalculated. An update stops wherever a recalculated value comes out the same.

```python
import watershed as ws

g = ws.Graph()
qty = g.input("qty", 10.0)
price = g.input("price", 2.5)
tax = g.input("tax_rate", 0.2)
subtotal = g.define("subtotal", qty * price)
total = g.define("total", subtotal * (1 + tax))

g.set(price, 3.0)
g.stabilize()            # recalculates subtotal and total, nothing else
print(total.value)       # 36.0
print(g.explain(total))  # why total has its value, as a tree
```

Collections are keyed tables whose totals update by only the rows that change:

```python
orders = g.collection("orders")
q = orders.field("qty", float)
p = orders.field("price", float)
line = orders.column("line", q * p)
revenue = orders.sum("revenue", line)    # also count, min, max
orders.upsert("o-1", qty=2, price=9.99)
orders.remove("o-7")
g.stabilize()
```

Formulas can be given as text, parsed by the same parser as the Rust API
and the grid:

```python
net = g.define("net", g.formula("qty * price * (1 - discount)"))
line = orders.column("line", orders.formula("qty * price * fx"))
```

## What the bindings cover

The package wraps [watershed-core](../watershed-core/README.md), which
documents each feature.

| Covered | Not exposed |
|---|---|
| Inputs, calculated nodes, `set`, `stabilize`, `get` | History (`keep_history`, `get_at`, `explain_at`) |
| `when(...).then(...).otherwise(...)` and `choose` | Change sinks, including Parquet |
| Values: `float`, `int`, `decimal.Decimal`, `bool`, `str` | `redefine` |
| Decimal rounding (`Expr.round`) and `to_float` | Scenario evaluation (watershed-gpu) |
| Collections: fields, columns, `sum`, `min`, `max`, `count`, `upsert`, `remove` | |
| Groupings: `group_sum`, `group_min`, `group_max`, `group_count` | |
| Formulas as text | |
| Pull mode (`watch`, `unwatch`, `calculate_only_watched`) and `remove` | |
| `explain`, `describe`, `to_dot`, `find`, `check_consistency` | |

Expression types are checked when an expression is built, at run time,
where the Rust API checks them at compile time. The type stubs are in
[`watershed.pyi`](watershed.pyi).

The package is not published to PyPI; build it from this folder. It needs
Python 3.9 or later.

## Developing

With [uv](https://docs.astral.sh/uv/), from this folder:

```bash
uv sync           # builds the Rust extension into .venv
uv run pytest
```
