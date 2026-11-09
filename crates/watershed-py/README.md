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

## Developing

With [uv](https://docs.astral.sh/uv/), from this folder:

```bash
uv sync           # builds the Rust extension into .venv
uv run pytest
```
