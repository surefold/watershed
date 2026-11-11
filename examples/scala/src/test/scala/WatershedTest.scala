import watershed.*

import scala.math.BigDecimal.RoundingMode

class WatershedTest extends munit.FunSuite:

  test("changing an input recalculates only what depends on it"):
    val g = Graph()
    val qty = g.input("qty", 10.0)
    val price = g.input("price", 2.5)
    val total: Node[Double] = g.define("total", qty * price)
    g.define("elsewhere", qty + 1)
    assertEquals(g(total), 25.0)

    g(price) = 3.0
    val report = g.stabilize()
    assertEquals(report.recalculated, 1L)
    assertEquals(g.lastRecalculated, List("total"))
    assertEquals(g(total), 30.0)
    assert(g.explain(total).startsWith("total = 30  ← qty * price"))
    g.checkConsistency()

  test("expressions of every type work out as written"):
    val g = Graph()
    val a = g.input("a", 6.0)
    val b = g.input("b", 4.0)
    val n = g.input("n", 7L)
    val on = g.input("on", true)
    val side = g.input("side", "Buy")

    assertEquals(g(g.define("d1", -a + b)), -2.0)
    assertEquals(g(g.define("d2", a + b * 2)), 14.0)
    assertEquals(g(g.define("d3", a / b / 2.0)), 0.75)
    assertEquals(g(g.define("d4", a.max(b) - a.min(b))), 2.0)
    assertEquals(g(g.define("d5", (b - a).abs)), 2.0)
    assertEquals(g(g.define("d6", 1 + a * 2)), 13.0)
    assertEquals(g(g.define("w1", n * 2 + 1)), 15L)
    assertEquals(g(g.define("w2", 10L - n)), 3L)
    assertEquals(g(g.define("f1", (a > b) && on)), true)
    assertEquals(g(g.define("f2", !on || (a === b))), false)
    assertEquals(g(g.define("i1", whenever(side === "Buy", 1.0, -1.0) * a)), 6.0)
    assertEquals(g(g.define("t1", side ++ " " ++ text(n))), "Buy 7")
    assertEquals(g(g.define("t2", texts(side, value("x")).join("-"))), "Buy-x")
    assertEquals(g(g.define("l1", texts(side) ++ texts(side))), List("Buy", "Buy"))
    assertEquals(g(g.define("c1", choose(n - 6, a, b))), 4.0)
    g.checkConsistency()

  test("what does not fit together does not compile"):
    val g = Graph()
    val qty = g.input("qty", 10.0)
    val n = g.input("n", 1L)
    val region = g.input("region", "EU")
    val total = g.define("total", qty * 2)
    val net = g.input("net", BigDecimal("1.50"))
    assert(compileErrors("""g.define("wrong", qty + region)""").nonEmpty, "a number plus a string")
    assert(compileErrors("""g.define("mixed", qty * n)""").nonEmpty, "a Double times a Long")
    assert(compileErrors("""g.define("money", net * qty)""").nonEmpty, "a BigDecimal times a Double")
    assert(compileErrors("""g.define("flag", qty && region)""").nonEmpty, "logic on numbers")
    assert(compileErrors("""g(total) = 5.0""").nonEmpty, "setting a calculated node")
    assert(compileErrors("""g(qty) = "three"""").nonEmpty, "setting a Double to a string")
    assert(compileErrors("""g.input("count", 3)""").nonEmpty, "an Int is not a type a node holds")
    // And what does fit, does.
    assertEquals(compileErrors("""g.define("fine", qty * 2 + total)"""), "")

  test("decimals are exact and round as asked"):
    val g = Graph()
    val a = g.input("a", BigDecimal("0.1"))
    val b = g.input("b", BigDecimal("0.2"))
    assertEquals(g(g.define("sum", a + b)), BigDecimal("0.3"))

    // 1.50 keeps its two places through a calculation.
    val price = g.input("price", BigDecimal("1.50"))
    val line = g.define("line", price * 3)
    assertEquals(g(line).toString, "4.50")

    val x = g.input("x", BigDecimal("2.345"))
    assertEquals(g(g.define("even", x.round(2))), BigDecimal("2.34"))
    assertEquals(g(g.define("up", x.round(2, RoundingMode.HALF_UP))), BigDecimal("2.35"))
    assertEquals(g(g.define("more", x > BigDecimal("2.3"))), true)

    val count = g.input("count", 3L)
    val total = g.define("total", count.toDecimal * price)
    assertEquals(g(total), BigDecimal("4.50"))
    assertEquals(g(g.define("rough", total.toDouble * 2.0)), 9.0)

    g(price) = BigDecimal("2.00")
    g.stabilize()
    assertEquals(g(total).toString, "6.00")

    val orders = g.collection("orders")
    val net = orders.decimal("net")
    val gross = orders.column("gross", (net * BigDecimal("1.20")).round(2))
    val revenue = orders.sum("revenue", gross)
    for i <- 0 until 1000 do orders.upsert(s"o$i", net := BigDecimal("0.10"))
    g.stabilize()
    assertEquals(g(revenue).toString, "120.00")

    // More digits than the engine holds is refused, not rounded.
    intercept[WatershedException](g.input("huge", BigDecimal("1e40")))
    g.checkConsistency()

  test("a collection's totals follow its rows"):
    val g = Graph()
    val orders = g.collection("orders")
    val qty = orders.number("qty")
    val region = orders.text("region")
    val double = orders.column("double", qty * 2)
    val total = orders.sum("total", double)
    val rows = orders.count("rows")
    orders.upsert("a", qty := 1.0, region := "EU")
    orders.upsert("b", qty := 4, region := "US")
    g.stabilize()
    assertEquals(g(total), 10.0)
    assertEquals(g(rows), 2L)

    orders.upsert("a", qty := 3.0)
    assertEquals(g.stabilize().recalculated, 2L)
    assertEquals(g(total), 14.0)
    assertEquals(orders("a", region), "EU")
    assertEquals(orders("a", double), 6.0)

    assert(orders.remove("b"))
    g.stabilize()
    assertEquals(g(total), 6.0)
    assertEquals(orders.size, 1L)
    g.checkConsistency()

  test("what the engine refuses comes with a reason"):
    val g = Graph()
    val a = g.input("a", 1.0)
    val again = intercept[WatershedException](g.input("a", 2.0))
    assertEquals(again.getMessage, "a node named \"a\" already exists")

    // A field can only be used in its own collection's columns.
    val qty = g.collection("orders").number("qty")
    intercept[WatershedException](g.define("stray", qty + a))

    // A calculation that fails is a value the node holds, not a crash.
    val zero = g.input("zero", 0L)
    val ratio = g.define("ratio", zero / zero)
    assertEquals(g.get(ratio), None)
    assert(intercept[WatershedException](g(ratio)).getMessage.contains("ratio"))

  test("many nodes and many prices can go in one call each"):
    val g = Graph()
    val prices = List.tabulate(100)(i => g.input(s"p$i", i.toDouble))
    val doubles = g.defineAll(prices.zipWithIndex.map((p, i) => s"d$i" -> p * 2))
    val total = g.define("total", doubles.map(d => d: Expr[Double]).reduce(_ + _))
    assertEquals(g(total), 9900.0)

    g.setAll(List(prices(0) -> 10.0, prices(1) -> 11.0))
    assertEquals(g.stabilize().inputsChanged, 2L)
    assertEquals(g(total), 9940.0)
    g.checkConsistency()

  test("a failure or a node of another graph is told apart from a value"):
    val g = Graph()
    val a = g.input("a", 1.0)
    val b = g.input("b", 0L)
    val half = g.define("half", a / 2.0)
    val broken = g.define("broken", (1L / b).toDecimal)
    val nan = g.define("nan", a - a + Double.NaN)

    g(a) = 5.0
    val report = g.stabilize()
    assertEquals(report.inputsChanged, 1L)
    assertEquals(report.recalculated, 2L)
    assertEquals(g(half), 2.5)

    // A node that really holds NaN is read as NaN, and one that failed says why.
    assert(g(nan).isNaN)
    assert(intercept[WatershedException](g(broken)).getMessage.contains("division by zero"))

    // A node of another graph is refused rather than read as this one's.
    val other = Graph()
    val theirs = other.input("theirs", 9.0)
    intercept[WatershedException](g(theirs))
    intercept[WatershedException](g(theirs) = 1.0)
    g.checkConsistency()

  test("every type of value is set and read back"):
    val g = Graph()
    val number = g.input("number", 1.5)
    val whole = g.input("whole", 7L)
    val decimal = g.input("decimal", BigDecimal("19.99"))
    val flag = g.input("flag", true)
    val words = g.input("text", "héllo, wörld")
    val list = g.define("list", texts(words, words))
    assertEquals(g(number), 1.5)
    assertEquals(g(whole), 7L)
    assertEquals(g(decimal), BigDecimal("19.99"))
    assertEquals(g(flag), true)
    assertEquals(g(words), "héllo, wörld")
    assertEquals(g(list), List("héllo, wörld", "héllo, wörld"))

    g(number) = -0.25
    g(whole) = Long.MinValue
    g(decimal) = BigDecimal("-79228162514264337593543950335")
    g(flag) = false
    g(words) = ""
    assertEquals(g.stabilize().inputsChanged, 5L)
    assertEquals(g(number), -0.25)
    assertEquals(g(whole), Long.MinValue)
    assertEquals(g(decimal), BigDecimal("-79228162514264337593543950335"))
    assertEquals(g(flag), false)
    assertEquals(g(words), "")

    g(decimal) = BigDecimal("0.0000000000000000000000000001")
    g(words) = "x" * 10_000
    g.stabilize()
    assertEquals(g(decimal).bigDecimal.toPlainString, "0.0000000000000000000000000001")
    assertEquals(g(words).length, 10_000)
    g.checkConsistency()

  test("many rows go in one call"):
    val g = Graph()
    val orders = g.collection("orders")
    val qty = orders.whole("qty")
    val price = orders.decimal("price")
    val note = orders.text("note")
    val line = orders.column("line", qty.toDecimal * price)
    val total = orders.sum("total", line)
    val count = orders.count("count")
    orders.upsertAll((0 until 1000).map(i => row(s"o$i", qty := i, price := BigDecimal("0.10"), note := s"row $i")))
    g.stabilize()
    assertEquals(g(count), 1000L)
    assertEquals(g(total).toString, "49950.00")
    assertEquals(orders("o7", note), "row 7")

    orders.upsertAll(Seq(row("o1", qty := 1001), row("o2", qty := 1002)))
    assertEquals(g.stabilize().recalculated, 3L)
    assertEquals(g(total).toString, "50150.00")

    // A row that is refused says why, and the rows before it stay.
    val refused = intercept[WatershedException]:
      orders.upsertAll(Seq(row("new", qty := 1, price := BigDecimal("1"), note := "ok"), row("bad", qty := 1)))
    assert(refused.getMessage.contains("needs every field"), refused.getMessage)
    g.stabilize()
    assertEquals(g(count), 1001L)
    g.checkConsistency()

  test("in pull mode only what is watched is calculated"):
    val g = Graph()
    val a = g.input("a", 1.0)
    val watched = g.define("watched", a + 1)
    val other = g.define("other", a + 2)
    g.watch(watched)
    g.calculateOnlyWatched(true)
    g(a) = 5
    assertEquals(g.stabilize().recalculated, 1L)
    assertEquals(g(watched), 6.0)
    intercept[WatershedException](g(other))

    g.calculateOnlyWatched(false)
    g.stabilize()
    assertEquals(g(other), 7.0)
