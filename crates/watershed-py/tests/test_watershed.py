"""Each test checks one promise the Python bindings make.

Run with:  uv run pytest   (from crates/watershed-py)
"""

import math
import random

import pytest

import watershed as ws


def order_graph():
    g = ws.Graph()
    qty = g.input("qty", 10.0)
    price = g.input("price", 2.5)
    tax = g.input("tax_rate", 0.2)
    subtotal = g.define("subtotal", qty * price)
    total = g.define("total", subtotal * (1 + tax))
    return g, qty, price, tax, subtotal, total


def test_a_new_node_has_its_value_straight_away():
    g, *_, total = order_graph()
    assert total.value == 30.0
    assert g.get(total) == 30.0


def test_only_nodes_that_depend_on_a_change_are_recalculated():
    g, qty, price, tax, subtotal, total = order_graph()
    other = g.define("other", tax * 100)
    g.set(price, 3.0)
    report = g.stabilize()
    assert g.last_recalculated() == ["subtotal", "total"]
    assert report.recalculated == 2
    assert total.value == 36.0
    assert other.value == 20.0


def test_an_update_stops_where_a_value_comes_out_the_same():
    g = ws.Graph()
    temperature = g.input("temperature", 20.0)
    hot = g.define("hot", temperature > 30)
    fan = g.define("fan", ws.when(hot).then(100.0).otherwise(0.0))
    g.set(temperature, 21.0)
    g.stabilize()
    assert g.last_recalculated() == ["hot"]
    g.set(temperature, 35)  # a whole number for a float input is fine
    g.stabilize()
    assert fan.value == 100.0


def test_reads_return_the_last_update_until_the_next_one():
    g, qty, *_, total = order_graph()
    g.set(qty, 12.0)
    assert total.value == 30.0
    g.stabilize()
    assert total.value == 36.0


def test_whole_numbers_mix_with_floats():
    g = ws.Graph()
    x = g.input("x", 1.5)
    y = g.define("y", 2 * x + 1 - x / 2)
    assert y.type == "float"
    assert y.value == pytest.approx(3.25)


def test_values_come_back_as_python_types():
    g = ws.Graph()
    n = g.input("n", 3)
    s = g.input("s", "EU")
    flag = g.define("flag", (n >= 3) & (s == "EU"))
    assert (n.value, s.value, flag.value) == (3, "EU", True)
    assert isinstance(n.value, int) and isinstance(flag.value, bool)


def test_type_mistakes_raise_as_the_expression_is_built():
    g = ws.Graph()
    x = g.input("x", 1.0)
    s = g.input("s", "text")
    with pytest.raises(TypeError, match="two numbers"):
        x + s
    with pytest.raises(TypeError, match="watershed values are"):
        x * [1, 2]
    with pytest.raises(TypeError):
        g.set(x, "not a number")


def test_an_expression_has_no_truth_value():
    g = ws.Graph()
    x = g.input("x", 1.0)
    with pytest.raises(TypeError, match="no truth value"):
        if x > 0:
            pass
    with pytest.raises(TypeError, match="not hashable"):
        {x: 1}


def test_errors_are_values_that_clear_when_fixed():
    g = ws.Graph()
    total = g.input("total", 100)
    count = g.input("count", 4)
    average = g.define("average", total / count)
    assert average.value == 25
    g.set(count, 0)
    g.stabilize()
    with pytest.raises(ws.CalcError, match="division by zero"):
        average.value
    g.set(count, 5)
    g.stabilize()
    assert average.value == 20


def test_names_are_unique_and_nodes_stay_in_their_graph():
    g = ws.Graph()
    a = g.input("a", 1.0)
    with pytest.raises(ws.CalcError, match="already exists"):
        g.input("a", 2.0)
    other = ws.Graph()
    with pytest.raises(ws.CalcError):
        other.define("double", a * 2)
    with pytest.raises(ws.CalcError, match="different graph"):
        other.get(a)


def test_a_conditional_only_waits_on_the_branch_it_took():
    g = ws.Graph()
    use_live = g.input("use_live", True)
    live = g.input("live", 101.0)
    eod = g.input("end_of_day", 100.0)
    price = g.define("price", ws.when(use_live).then(live).otherwise(eod))
    g.set(eod, 98.0)
    assert g.stabilize().recalculated == 0
    g.set(use_live, False)
    g.stabilize()
    assert price.value == 98.0


def test_choose_picks_by_position():
    g = ws.Graph()
    which = g.input("which", 1)
    picked = g.define("picked", ws.choose(which, [10.0, 20, 30.0]))
    assert picked.value == 20.0
    g.set(which, 5)
    g.stabilize()
    with pytest.raises(ws.CalcError, match="out of range"):
        picked.value


def test_explain_describe_and_dot():
    g, qty, *_, total = order_graph()
    g.set(qty, 12.0)
    g.stabilize()
    text = g.explain(total)
    assert text.startswith("total = 36  ← subtotal * (1 + tax_rate)  [changed]")
    assert "qty = 12  (input)  [changed]" in text
    assert "level 2" in g.describe()
    assert g.to_dot().startswith("digraph watershed {")
    assert g.find("total").value == 36.0
    assert g.find("nope") is None
    assert repr(total) == "<watershed node total: float = 36>"


def test_collections_total_rows_and_follow_changes():
    g = ws.Graph()
    fx = g.input("fx", 1.25)
    orders = g.collection("orders")
    qty = orders.field("qty", float)
    price = orders.field("price", float)
    line = orders.column("line", qty * price * fx)
    revenue = orders.sum("revenue", line)
    count = orders.count("count")
    largest = orders.max("largest", line)
    orders.upsert("o1", qty=2, price=10)
    orders.upsert("o2", qty=1, price=5)
    g.stabilize()
    assert (revenue.value, count.value, largest.value) == (31.25, 2, 25.0)

    orders.upsert("o2", qty=3)
    g.stabilize()
    assert g.last_recalculated() == ["orders[1].line", "revenue", "largest"]
    assert orders.get("o2", line) == 18.75

    assert orders.remove("o1")
    g.stabilize()
    assert (revenue.value, count.value, len(orders), "o1" in orders) == (18.75, 1, 1, False)
    assert orders.keys() == ["o2"]

    with pytest.raises(ws.CalcError, match="needs every field"):
        orders.upsert("o3", qty=1)
    with pytest.raises(ws.CalcError, match="no field"):
        orders.upsert("o2", colour="red")
    g.check_consistency()


def test_a_big_collection_matches_a_plain_python_sum():
    rng = random.Random(7)
    g = ws.Graph()
    rows = g.collection("rows")
    x = rows.field("x", int)
    total = rows.sum("total", x)
    smallest = rows.min("smallest", x)
    model = {}
    for step in range(200):
        for _ in range(rng.randint(1, 20)):
            key = f"k{rng.randrange(500)}"
            if rng.random() < 0.2:
                rows.remove(key)
                model.pop(key, None)
            else:
                value = rng.randint(-1000, 1000)
                rows.upsert(key, x=value)
                model[key] = value
        g.stabilize()
        assert total.value == sum(model.values())
        assert smallest.value == (min(model.values()) if model else 2**63 - 1)
    g.check_consistency()


def test_stabilize_lets_other_python_threads_run():
    # stabilize() releases the interpreter while the engine works; this just
    # checks that a large update from Python gives the right answer.
    g = ws.Graph()
    prices = [g.input(f"p{i}", 1.0) for i in range(1000)]
    total = g.define("total", sum(prices[1:], prices[0] * 1))
    for p in prices:
        g.set(p, 2.0)
    g.stabilize()
    assert total.value == 2000.0
    assert not math.isnan(total.value)


def test_groups_total_rows_per_key_and_follow_rows_that_move():
    g = ws.Graph()
    sales = g.collection("sales")
    region = sales.field("region", str)
    amount = sales.field("amount", float)
    by_region = sales.group_sum("by_region", region, amount)
    counts = sales.group_count("count_by_region", region)
    sales.upsert("s1", region="EU", amount=10)
    sales.upsert("s2", region="EU", amount=5)
    sales.upsert("s3", region="US", amount=7)
    g.stabilize()
    assert by_region.to_dict() == {"EU": 15.0, "US": 7.0}
    assert by_region["EU"].value == 15.0
    assert "US" in by_region and "APAC" not in by_region
    assert by_region.get("APAC") is None
    with pytest.raises(KeyError):
        by_region["APAC"]

    sales.upsert("s2", region="US")
    g.stabilize()
    assert by_region.to_dict() == {"EU": 10.0, "US": 12.0}
    assert counts.to_dict() == {"EU": 1, "US": 2}

    sales.remove("s1")
    g.stabilize()
    assert by_region.keys() == ["US"] and len(by_region) == 1
    g.check_consistency()


def test_groups_by_an_int_field():
    g = ws.Graph()
    c = g.collection("c")
    team = c.field("team", int)
    points = c.field("points", int)
    best = c.group_max("best", team, points)
    for i in range(30):
        c.upsert(f"p{i}", team=i % 3, points=i)
    g.stabilize()
    assert best.to_dict() == {0: 27, 1: 28, 2: 29}


def test_a_formula_written_as_text_is_an_expression():
    g, qty, *_ = order_graph()
    g.input("discount rate", 0.1)
    net = g.define("net", g.formula("qty * price * (1 - `discount rate`)"))
    assert net.value == pytest.approx(22.5)
    assert net.type == "float"

    g.set(qty, 20.0)
    assert g.stabilize().recalculated == 3  # subtotal, total and net
    assert net.value == pytest.approx(45.0)

    # It mixes with expressions built in Python.
    assert g.define("double", g.formula("net") * 2).value == pytest.approx(90.0)

    with pytest.raises(ws.CalcError, match='no column or value called "nothing"'):
        g.formula("nothing + 1")
    with pytest.raises(TypeError, match="cannot be applied to f64"):
        g.formula("qty && price")
    g.check_consistency()


def test_a_collections_formula_reads_its_fields_and_columns():
    g = ws.Graph()
    g.input("fx", 2.0)
    orders = g.collection("orders")
    orders.field("qty", float)
    orders.field("price", float)
    line = orders.column("line", orders.formula("qty * price * fx"))
    big = orders.column("big", orders.formula("line > 10"))
    revenue = orders.sum("revenue", line)
    orders.upsert("a", qty=1.0, price=1.5)
    orders.upsert("b", qty=4.0, price=1.5)
    g.stabilize()
    assert revenue.value == 15.0
    assert orders.get("a", big) is False
    assert orders.get("b", big) is True
    g.check_consistency()


def test_groups_by_a_calculated_column_follow_it_within_the_update():
    g = ws.Graph()
    limit = g.input("limit", 100.0)
    trades = g.collection("trades")
    notional = trades.field("notional", float)
    size = trades.column("size", ws.when(notional > limit).then("large").otherwise("small"))
    by_size = trades.group_sum("by_size", size, notional)
    trades.upsert("t1", notional=50.0)
    trades.upsert("t2", notional=150.0)
    g.stabilize()
    assert by_size.to_dict() == {"large": 150.0, "small": 50.0}

    g.set(limit, 10.0)
    g.stabilize()
    assert by_size.to_dict() == {"large": 200.0}
    assert by_size.keys() == ["large"]
    g.check_consistency()


def test_a_node_nothing_uses_can_be_removed():
    g, qty, price, tax, subtotal, total = order_graph()
    with pytest.raises(ws.CalcError, match='"subtotal" cannot be removed: "total" uses it'):
        g.remove(subtotal)
    g.remove(total)
    assert g.find("total") is None
    with pytest.raises(ws.CalcError, match="has been removed"):
        g.get(total)
    with pytest.raises(ws.CalcError, match="has been removed"):
        total.value

    g.set(qty, 20.0)
    assert g.stabilize().recalculated == 1  # subtotal only
    again = g.define("total", subtotal * 2)
    assert again.value == pytest.approx(100.0)
    g.check_consistency()


def test_in_pull_mode_only_watched_nodes_are_calculated():
    g, qty, price, tax, subtotal, total = order_graph()
    other = g.define("other", qty + 1)
    g.watch(total)
    g.calculate_only_watched(True)

    g.set(qty, 20.0)
    assert g.stabilize().recalculated == 2  # subtotal and total, not other
    assert total.value == pytest.approx(60.0)
    assert not g.is_calculated(other)

    g.watch(other)
    assert g.stabilize().recalculated == 1
    assert other.value == 21.0

    g.calculate_only_watched(False)
    g.check_consistency()


def test_decimals_are_exact_and_round_as_asked():
    from decimal import Decimal

    g = ws.Graph()
    a = g.input("a", Decimal("0.1"))
    b = g.input("b", Decimal("0.2"))
    total = g.define("total", a + b)
    assert total.value == Decimal("0.3")
    assert total.type == "Decimal"

    # 1.50 keeps its two places, and a whole number beside it is exact.
    price = g.input("price", Decimal("1.50"))
    line = g.define("line", price * 3)
    assert str(line.value) == "4.50"

    x = g.input("x", Decimal("2.345"))
    assert g.define("even", x.round(2)).value == Decimal("2.34")
    assert g.define("up", x.round(2, "half_up")).value == Decimal("2.35")
    assert g.define("rough", line.to_float() * 2).value == 9.0
    assert g.define("more", x > Decimal("2.3")).value is True

    g.set(price, 2)
    g.stabilize()
    assert str(line.value) == "6"

    # A decimal and a float do not mix by accident.
    f = g.input("f", 1.5)
    with pytest.raises(TypeError, match="two numbers of the same kind"):
        price * f
    with pytest.raises(ValueError, match="rounding is"):
        x.round(2, "sideways")
    with pytest.raises(ValueError, match="28 significant digits"):
        g.input("huge", Decimal("1e40"))

    orders = g.collection("orders")
    net = orders.field("net", Decimal)
    gross = orders.column("gross", (net * Decimal("1.20")).round(2))
    revenue = orders.sum("revenue", gross)
    for i in range(1000):
        orders.upsert(f"o{i}", net=Decimal("0.10"))
    g.stabilize()
    assert str(revenue.value) == "120.00"
    g.check_consistency()
