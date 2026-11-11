import dev.surefold.watershed.Graph
import dev.surefold.watershed.Kind
import dev.surefold.watershed.Value
import dev.surefold.watershed.WatershedException

fun number(value: Double) = Value.Number(value)

fun main() {
    // ---------------------------------------------------------------- a graph
    val g = Graph()
    val qty = g.input("qty", number(10.0))
    val price = g.input("price", number(2.5))
    g.input("tax rate", number(0.2))
    val subtotal = g.define("subtotal", "qty * price")
    val total = g.define("total", "subtotal * (1 + `tax rate`)")
    val shipping = g.define("shipping", "if(qty > 20, 0, 4.99)")

    println("total = ${g.get(total)}")

    // Change one input: only what depends on it is recalculated.
    g.set(price, number(3.0))
    var report = g.stabilize()
    println("price 2.5 -> 3.0 recalculated ${g.lastRecalculated()}, not shipping")
    println("total = ${g.get(total)}")
    println()
    println(g.explain(total))

    // An update stops where a value comes out the same: qty 10 -> 12 leaves
    // shipping at 4.99, so nothing above shipping would be told.
    g.set(qty, number(12.0))
    report = g.stabilize()
    println("qty 10 -> 12: ${report.recalculated} recalculated, ${report.changed} changed")
    println("shipping = ${g.get(shipping)}, subtotal = ${g.get(subtotal)}")
    println()

    // Mistakes are refused with a reason.
    try {
        g.define("oops", "qty * nothing")
    } catch (e: WatershedException) {
        println("refused: ${e.message}")
    }
    try {
        g.define("mixed", "qty && price")
    } catch (e: WatershedException) {
        println("refused: ${e.message}")
    }
    println()

    // ------------------------------------------------------------ a collection
    // A keyed table of rows. Every cell is a node, and totals update by only
    // the rows that change.
    val fx = g.input("fx", number(1.25))
    val orders = g.collection("orders")
    orders.field("qty", Kind.NUMBER)
    orders.field("price", Kind.NUMBER)
    orders.column("line", "qty * price * fx")
    val revenue = orders.sum("revenue", "line")
    val largest = orders.max("largest", "line")
    val count = orders.count("orders")

    val rows = 100_000
    for (i in 0 until rows) {
        orders.upsert("o$i", mapOf("qty" to number((i % 100).toDouble()), "price" to number(1.5)))
    }
    report = g.stabilize()
    println("$rows rows: ${g.size()} nodes, revenue = ${g.get(revenue)}, rows = ${g.get(count)}")

    orders.upsert("o42", mapOf("qty" to number(1000.0)))
    report = g.stabilize()
    println(
        "one row changed: ${report.recalculated} nodes recalculated " +
            "(its line and two totals), largest = ${g.get(largest)}",
    )

    g.set(fx, number(1.3))
    report = g.stabilize()
    println("the exchange rate changed: ${report.recalculated} nodes recalculated (every line)")
    println("row o42's line = ${orders.get("o42", "line")}")

    g.checkConsistency()
    println("checked: every node matches a calculation from scratch")
}
