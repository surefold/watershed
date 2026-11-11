import dev.surefold.watershed.*
import java.math.BigDecimal
import java.math.RoundingMode
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class WatershedTest {
    @Test
    fun `changing an input recalculates only what depends on it`() {
        val g = Graph()
        val qty = g.input("qty", 10.0)
        val price = g.input("price", 2.5)
        val total: Node<Double> = g.define("total", qty * price)
        g.define("elsewhere", qty + 1)
        assertEquals(25.0, g[total])

        g[price] = 3.0
        val report = g.stabilize()
        assertEquals(1L, report.recalculated)
        assertEquals(listOf("total"), g.lastRecalculated())
        assertEquals(30.0, g[total])
        assertTrue(g.explain(total).startsWith("total = 30  ← qty * price"))
        g.checkConsistency()
    }

    @Test
    fun `expressions of every type work out as written`() {
        val g = Graph()
        val a = g.input("a", 6.0)
        val b = g.input("b", 4.0)
        val n = g.input("n", 7L)
        val on = g.input("on", true)
        val side = g.input("side", "Buy")

        assertEquals(-2.0, g[g.define("d1", -a + b)])
        val d2 = g.define("d2", a + b * 2)
        assertEquals(14.0, g[d2])
        assertEquals(0.75, g[g.define("d3", a / b / 2.0)])
        assertEquals(2.0, g[g.define("d4", (a greatest b) - (a least b))])
        assertEquals(2.0, g[g.define("d5", abs(b - a))])
        assertEquals(15L, g[g.define("w1", n * 2 + 1)])
        assertEquals(3L, g[g.define("w2", 10L - n)])
        assertEquals(true, g[g.define("f1", (a gt b) and on)])
        assertEquals(false, g[g.define("f2", !on or (a eq b))])
        assertEquals(6.0, g[g.define("i1", whenever(side eq "Buy", 1.0, -1.0) * a)])
        assertEquals("Buy 7", g[g.define("t1", side + " " + text(n))])
        assertEquals("Buy-x", g[g.define("t2", texts(side, value("x")).join("-"))])
        assertEquals(listOf("Buy", "Buy"), g[g.define("l1", texts(side) + texts(side))])
        assertEquals(4.0, g[g.define("c1", choose(n - 6, a, b))])

        // Each one explains itself as the engine sees it.
        assertEquals("d2 = 14  ← a + (b * 2)", g.explain(d2).lines().first())
        g.checkConsistency()
    }

    @Test
    fun `decimals are exact and round as asked`() {
        val g = Graph()
        val a = g.input("a", BigDecimal("0.1"))
        val b = g.input("b", BigDecimal("0.2"))
        val sum = g.define("sum", a + b)
        assertEquals(BigDecimal("0.3"), g[sum])

        // 1.50 keeps its two places through a calculation.
        val price = g.input("price", BigDecimal("1.50"))
        val line = g.define("line", price * 3)
        assertEquals("4.50", g[line].toPlainString())

        val x = g.input("x", BigDecimal("2.345"))
        assertEquals(BigDecimal("2.34"), g[g.define("even", x.round(2))])
        assertEquals(BigDecimal("2.35"), g[g.define("up", x.round(2, RoundingMode.HALF_UP))])
        assertEquals("0.3333333333333333333333333333", g[g.define("third", 1 / (x - x + 3))].toPlainString())
        assertEquals(true, g[g.define("more", x gt BigDecimal("2.3"))])

        val count = g.input("count", 3L)
        val total = g.define("total", count.toDecimal() * price)
        assertEquals(BigDecimal("4.50"), g[total])
        assertEquals(9.0, g[g.define("rough", total.toDouble() * 2.0)])

        g[price] = BigDecimal("2.00")
        g.stabilize()
        assertEquals(BigDecimal("6.00"), g[total])

        val orders = g.collection("orders")
        val net = orders.decimal("net")
        val gross = orders.column("gross", (net * BigDecimal("1.20")).round(2))
        val revenue = orders.sum("revenue", gross)
        repeat(1000) { orders.upsert("o$it", net(BigDecimal("0.10"))) }
        g.stabilize()
        assertEquals("120.00", g[revenue].toPlainString())

        // More digits than the engine holds is refused, not rounded.
        assertFailsWith<WatershedException> { g.input("huge", BigDecimal("1e40")) }
        g.checkConsistency()
    }

    @Test
    fun `a collection's totals follow its rows`() {
        val g = Graph()
        val orders = g.collection("orders")
        val qty = orders.number("qty")
        val region = orders.text("region")
        val double = orders.column("double", qty * 2)
        val total = orders.sum("total", double)
        val rows = orders.count("rows")
        orders.upsert("a", qty(1.0), region("EU"))
        orders.upsert("b", qty(4), region("US"))
        g.stabilize()
        assertEquals(10.0, g[total])
        assertEquals(2L, g[rows])

        orders.upsert("a", qty(3.0))
        assertEquals(2L, g.stabilize().recalculated)
        assertEquals(14.0, g[total])
        assertEquals("EU", orders["a", region])
        assertEquals(6.0, orders["a", double])

        assertTrue(orders.remove("b"))
        g.stabilize()
        assertEquals(6.0, g[total])
        assertEquals(1L, orders.size())
        g.checkConsistency()
    }

    @Test
    fun `what the engine refuses comes with a reason`() {
        val g = Graph()
        val a = g.input("a", 1.0)
        val again = assertFailsWith<WatershedException> { g.input("a", 2.0) }
        assertEquals("a node named \"a\" already exists", again.message)

        // A field can only be used in its own collection's columns.
        val qty = g.collection("orders").number("qty")
        assertFailsWith<WatershedException> { g.define("stray", qty + a) }

        // A calculation that fails is a value the node holds, not a crash.
        val zero = g.input("zero", 0L)
        val ratio = g.define("ratio", zero / zero)
        assertNull(g.getOrNull(ratio))
        assertTrue(assertFailsWith<WatershedException> { g[ratio] }.message!!.contains("ratio"))
    }

    @Test
    fun `many nodes and many prices can go in one call each`() {
        val g = Graph()
        val prices = List(100) { g.input("p$it", it.toDouble()) }
        val doubles = g.defineAll(prices.mapIndexed { i, p -> "d$i" to p * 2 })
        val total = g.define("total", doubles.reduce<Expr<Double>, Node<Double>> { sum, d -> sum + d })
        assertEquals(9900.0, g[total])

        g.setAll(mapOf(prices[0] to 10.0, prices[1] to 11.0))
        assertEquals(1, g.stabilize().inputsChanged.toInt() - 1)
        assertEquals(9940.0, g[total])
        g.checkConsistency()
    }

    @Test
    fun `a failure or a node of another graph is told apart from a value`() {
        val g = Graph()
        val a = g.input("a", 1.0)
        val b = g.input("b", 0L)
        val half = g.define("half", a / 2.0)
        val broken = g.define("broken", (1L / b).toDecimal())
        val nan = g.define("nan", a - a + Double.NaN)

        // A Double is set, updated and read directly.
        g[a] = 5.0
        val report = g.stabilize()
        assertEquals(1L, report.inputsChanged)
        assertEquals(2L, report.recalculated)
        assertEquals(2.5, g[half])

        // A node that really holds NaN is read as NaN, and one that
        // failed says why.
        assertTrue(g[nan].isNaN())
        assertTrue(assertFailsWith<WatershedException> { g[broken] }.message!!.contains("division by zero"))

        // A node of another graph is refused rather than read as this one's.
        val other = Graph()
        val theirs = other.input("theirs", 9.0)
        assertFailsWith<WatershedException> { g[theirs] }
        assertFailsWith<WatershedException> { g[theirs] = 1.0 }
        g.checkConsistency()
    }

    @Test
    fun `every type of value is set and read back`() {
        val g = Graph()
        val number = g.input("number", 1.5)
        val whole = g.input("whole", 7L)
        val decimal = g.input("decimal", BigDecimal("19.99"))
        val flag = g.input("flag", true)
        val text = g.input("text", "héllo, wörld")
        val list = g.define("list", texts(text, text))
        assertEquals(1.5, g[number])
        assertEquals(7L, g[whole])
        assertEquals(BigDecimal("19.99"), g[decimal])
        assertEquals(true, g[flag])
        assertEquals("héllo, wörld", g[text])
        assertEquals(listOf("héllo, wörld", "héllo, wörld"), g[list])

        g[number] = -0.25
        g[whole] = Long.MIN_VALUE
        g[decimal] = BigDecimal("-79228162514264337593543950335")
        g[flag] = false
        g[text] = ""
        assertEquals(5L, g.stabilize().inputsChanged)
        assertEquals(-0.25, g[number])
        assertEquals(Long.MIN_VALUE, g[whole])
        assertEquals(BigDecimal("-79228162514264337593543950335"), g[decimal])
        assertEquals(false, g[flag])
        assertEquals("", g[text])
        assertEquals(listOf("", ""), g[list])

        // The smallest step a decimal has, and a long text.
        g[decimal] = BigDecimal("0.0000000000000000000000000001")
        g[text] = "x".repeat(10_000)
        g.stabilize()
        assertEquals("0.0000000000000000000000000001", g[decimal].toPlainString())
        assertEquals(10_000, g[text].length)
        assertEquals(listOf("list"), g.lastRecalculated())
        g.checkConsistency()
    }

    @Test
    fun `many rows go in one call`() {
        val g = Graph()
        val orders = g.collection("orders")
        val qty = orders.whole("qty")
        val price = orders.decimal("price")
        val note = orders.text("note")
        val line = orders.column("line", qty.toDecimal() * price)
        val total = orders.sum("total", line)
        val count = orders.count("count")
        orders.upsertAll(List(1000) { row("o$it", qty(it), price(BigDecimal("0.10")), note("row $it")) })
        g.stabilize()
        assertEquals(1000L, g[count])
        assertEquals("49950.00", g[total].toPlainString())
        assertEquals("row 7", orders["o7", note])

        // Changing some rows changes only those.
        orders.upsertAll(listOf(row("o1", qty(1001)), row("o2", qty(1002))))
        assertEquals(3L, g.stabilize().recalculated)
        assertEquals("50150.00", g[total].toPlainString())

        // A row that is refused says why, and the rows before it stay.
        val refused = assertFailsWith<WatershedException> {
            orders.upsertAll(listOf(row("new", qty(1), price(BigDecimal("1")), note("ok")), row("bad", qty(1))))
        }
        assertTrue(refused.message!!.contains("needs every field"), refused.message)
        g.stabilize()
        assertEquals(1001L, g[count])
        g.checkConsistency()
    }

    @Test
    fun `in pull mode only what is watched is calculated`() {
        val g = Graph()
        val a = g.input("a", 1.0)
        val watched = g.define("watched", a + 1)
        val other = g.define("other", a + 2)
        g.watch(watched)
        g.calculateOnlyWatched(true)
        g[a] = 5
        assertEquals(1L, g.stabilize().recalculated)
        assertEquals(6.0, g[watched])
        assertFailsWith<WatershedException> { g[other] }

        g.calculateOnlyWatched(false)
        g.stabilize()
        assertEquals(7.0, g[other])
    }
}
