import dev.surefold.watershed.*
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
        assertEquals(1uL, report.recalculated)
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
        assertEquals(14.0, g[g.define("d2", a + b * 2)])
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
        assertEquals("d2 = 14  ← a + (b * 2)", g.explain(g.find("d2")!!).lines().first())
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
        assertEquals(2uL, g.stabilize().recalculated)
        assertEquals(14.0, g[total])
        assertEquals("EU", orders["a", region])
        assertEquals(6.0, orders["a", double])

        assertTrue(orders.remove("b"))
        g.stabilize()
        assertEquals(6.0, g[total])
        assertEquals(1uL, orders.size())
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
    fun `in pull mode only what is watched is calculated`() {
        val g = Graph()
        val a = g.input("a", 1.0)
        val watched = g.define("watched", a + 1)
        val other = g.define("other", a + 2)
        g.watch(watched)
        g.calculateOnlyWatched(true)
        g[a] = 5
        assertEquals(1uL, g.stabilize().recalculated)
        assertEquals(6.0, g[watched])
        assertFailsWith<WatershedException> { g[other] }

        g.calculateOnlyWatched(false)
        g.stabilize()
        assertEquals(7.0, g[other])
    }
}
