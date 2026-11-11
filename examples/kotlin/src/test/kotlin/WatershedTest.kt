import dev.surefold.watershed.Graph
import dev.surefold.watershed.Kind
import dev.surefold.watershed.Value
import dev.surefold.watershed.WatershedException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class WatershedTest {
    @Test
    fun `changing an input recalculates only what depends on it`() {
        val g = Graph()
        val qty = g.input("qty", Value.Number(10.0))
        val price = g.input("price", Value.Number(2.5))
        val total = g.define("total", "qty * price")
        g.define("elsewhere", "qty + 1")
        assertEquals(Value.Number(25.0), g.get(total))

        g.set(price, Value.Number(3.0))
        val report = g.stabilize()
        assertEquals(1uL, report.recalculated)
        assertEquals(listOf("total"), g.lastRecalculated())
        assertEquals(Value.Number(30.0), g.get(total))
        assertTrue(g.explain(total).startsWith("total = 30"))
        g.checkConsistency()
        assertEquals(qty, g.find("qty"))
    }

    @Test
    fun `a collection's totals follow its rows`() {
        val g = Graph()
        val orders = g.collection("orders")
        orders.field("qty", Kind.NUMBER)
        orders.field("region", Kind.TEXT)
        orders.column("double", "qty * 2")
        val total = orders.sum("total", "double")
        orders.upsert("a", mapOf("qty" to Value.Number(1.0), "region" to Value.Text("EU")))
        orders.upsert("b", mapOf("qty" to Value.Number(4.0), "region" to Value.Text("US")))
        g.stabilize()
        assertEquals(Value.Number(10.0), g.get(total))

        orders.upsert("a", mapOf("qty" to Value.Number(3.0)))
        assertEquals(2uL, g.stabilize().recalculated)
        assertEquals(Value.Number(14.0), g.get(total))
        assertEquals(Value.Text("EU"), orders.get("a", "region"))

        assertTrue(orders.remove("b"))
        g.stabilize()
        assertEquals(Value.Number(6.0), g.get(total))
        assertEquals(1uL, orders.size())
        g.checkConsistency()
    }

    @Test
    fun `mistakes are refused with a reason`() {
        val g = Graph()
        val a = g.input("a", Value.Number(1.0))
        val refused = assertFailsWith<WatershedException> { g.define("b", "a + nothing") }
        assertEquals("there is no column or value called \"nothing\"", refused.message)
        assertFailsWith<WatershedException> { g.set(a, Value.Text("one")) }
        assertFailsWith<WatershedException> { g.get(999u) }
        // A calculation that fails is a value, not an exception.
        g.input("zero", Value.Whole(0))
        val ratio = g.define("ratio", "zero / zero")
        assertTrue(g.get(ratio) is Value.Failed)
    }

    @Test
    fun `in pull mode only what is watched is calculated`() {
        val g = Graph()
        val a = g.input("a", Value.Number(1.0))
        val watched = g.define("watched", "a + 1")
        val other = g.define("other", "a + 2")
        g.watch(watched)
        g.calculateOnlyWatched(true)
        g.set(a, Value.Number(5.0))
        assertEquals(1uL, g.stabilize().recalculated)
        assertEquals(Value.Number(6.0), g.get(watched))
        assertFailsWith<WatershedException> { g.get(other) }

        g.calculateOnlyWatched(false)
        g.stabilize()
        assertEquals(Value.Number(7.0), g.get(other))
    }
}
