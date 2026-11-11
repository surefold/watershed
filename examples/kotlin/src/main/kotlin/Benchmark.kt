import dev.surefold.watershed.*
import java.math.BigDecimal
import kotlin.time.Duration
import kotlin.time.measureTime

// The engine's benchmark, driven from Kotlin: the same graph as
// `cargo run --release --example benchmark` (market prices → trades → a tree
// of subtotals), to see what crossing from the JVM adds.
//
//     gradle benchmark                  500,000 trades, about a million nodes
//     gradle benchmark --args=100000    another number of trades

private const val MARKET_PRICES = 10_000
private const val BATCH = 10_000

/** A small deterministic random number generator, so every run builds the same graph. */
private class Rng(private var state: ULong) {
    fun next(): ULong {
        state = state xor (state shl 13)
        state = state xor (state shr 7)
        state = state xor (state shl 17)
        return state
    }
    fun below(n: Int): Int = (next() % n.toULong()).toInt()
    fun unit(): Double = (next() % 1_000_000uL).toDouble() / 1_000_000.0
}

private fun median(runs: Int, step: () -> Unit): Duration =
    List(runs) { measureTime(step) }.sorted()[runs / 2]

private fun show(d: Duration): String {
    val nanos = d.inWholeNanoseconds
    return when {
        nanos < 1_000 -> "$nanos ns"
        nanos < 1_000_000 -> "%.1f µs".format(nanos / 1000.0)
        else -> "%.1f ms".format(nanos / 1_000_000.0)
    }
}

fun main(args: Array<String>) {
    val trades = args.firstOrNull()?.replace("_", "")?.toInt() ?: 500_000
    val rng = Rng(0x2545_F491_4F6C_DD1DuL)
    val g = Graph()

    lateinit var prices: List<Input<Double>>
    lateinit var total: Node<Double>
    val build = measureTime {
        prices = List(MARKET_PRICES) { g.input("price$it", 100.0 + rng.unit()) }
        // Thousands of nodes go in one call each time, not one call a node.
        var level: List<Node<Double>> = (0 until trades).chunked(BATCH).flatMap { batch ->
            g.defineAll(batch.map { t ->
                val a = prices[rng.below(MARKET_PRICES)]
                val b = prices[rng.below(MARKET_PRICES)]
                "trade$t" to a * (rng.unit() * 10.0) + b * (rng.unit() * -5.0)
            })
        }
        var depth = 0
        while (level.size > 1) {
            depth += 1
            val pairs = level.chunked(2)
            val sums = pairs.withIndex().filter { it.value.size == 2 }.chunked(BATCH).flatMap { batch ->
                g.defineAll(batch.map { (k, pair) -> "sum${depth}_$k" to pair[0] + pair[1] })
            }
            // An odd one out goes up a level as it is.
            level = if (pairs.last().size == 1) sums + pairs.last()[0] else sums
        }
        total = level[0]
    }
    val nodes = g.size().toLong()
    println("graph: $nodes nodes built from Kotlin in ${show(build)} (${"%.1f".format(build.inWholeNanoseconds / 1000.0 / nodes)} µs a node)")
    println()

    // Warm the JVM up on the calls being timed.
    repeat(2_000) {
        g[prices[rng.below(MARKET_PRICES)]] = 100.0 + rng.unit()
        g.stabilize()
    }

    fun change(count: Int) {
        if (count == 1) {
            g[prices[rng.below(MARKET_PRICES)]] = 100.0 + rng.unit()
        } else {
            g.setAll(List(count) { prices[rng.below(MARKET_PRICES)] to 100.0 + rng.unit() }.toMap())
        }
        g.stabilize()
    }
    val one = median(2_000) { change(1) }
    val some = median(50) { change(MARKET_PRICES / 100) }
    val many = median(15) { change(MARKET_PRICES / 10) }
    val quiet = Graph()
    val lone = quiet.input("lone", 1.0)
    var tick = 0.0

    println("%-46s %12s".format("median of many runs, from Kotlin", "time"))
    println("%-46s %12s".format("incremental: one price changes", show(one)))
    println("%-46s %12s".format("incremental: 1% of prices change together", show(some)))
    println("%-46s %12s".format("incremental: 10% of prices change together", show(many)))
    println()

    // What each kind of value costs to set and to read, and what a node
    // or a row costs to add, one at a time and many at once.
    val whole = quiet.input("whole", 0L)
    val decimal = quiet.input("decimal", BigDecimal("0.00"))
    val text = quiet.input("text", "")
    val cents = List(1000) { BigDecimal(it).movePointLeft(2) }
    val words = List(1000) { "value $it" }
    var n = 0
    fun each(step: () -> Unit): Duration {
        repeat(50_000) { n += 1; step() }
        return median(50_000) { n += 1; step() }
    }
    println("%-46s %12s".format("one call, from Kotlin", "time"))
    println("%-46s %12s".format("set a Double", show(each { quiet[lone] = tick++ })))
    println("%-46s %12s".format("read a Double", show(each { quiet[lone] })))
    println("%-46s %12s".format("an update with nothing to do", show(each { quiet.stabilize() })))
    println("%-46s %12s".format("set a Long", show(each { quiet[whole] = n.toLong() })))
    println("%-46s %12s".format("read a Long", show(each { quiet[whole] })))
    println("%-46s %12s".format("set a BigDecimal", show(each { quiet[decimal] = cents[n % 1000] })))
    println("%-46s %12s".format("read a BigDecimal", show(each { quiet[decimal] })))
    println("%-46s %12s".format("set a String", show(each { quiet[text] = words[n % 1000] })))
    println("%-46s %12s".format("read a String", show(each { quiet[text] })))
    println()

    val count = 200_000
    fun perItem(total: Duration) = "%.2f µs each".format(total.inWholeNanoseconds / 1000.0 / count)
    val a = Graph()
    val base = a.input("base", 1.0)
    val oneByOne = measureTime { repeat(count) { a.define("n$it", base * it.toDouble() + 1.0) } }
    val b = Graph()
    val base2 = b.input("base", 1.0)
    val together = measureTime {
        (0 until count).chunked(BATCH).forEach { batch -> b.defineAll(batch.map { "n$it" to base2 * it.toDouble() + 1.0 }) }
    }
    println("%-46s %14s".format("$count nodes, one define each", perItem(oneByOne)))
    println("%-46s %14s".format("$count nodes, defineAll in batches", perItem(together)))

    fun table(g: Graph): Triple<dev.surefold.watershed.Collection, Field<Double>, Field<BigDecimal>> {
        val orders = g.collection("orders")
        val qty = orders.number("qty")
        val price = orders.decimal("price")
        orders.column("line", qty * 2.0)
        return Triple(orders, qty, price)
    }
    val c = Graph()
    val (rowsOne, qtyOne, priceOne) = table(c)
    val rowByRow = measureTime { repeat(count) { rowsOne.upsert("o$it", qtyOne(it), priceOne(cents[it % 1000])) } }
    val d = Graph()
    val (rowsAll, qtyAll, priceAll) = table(d)
    val rowsTogether = measureTime {
        (0 until count).chunked(BATCH).forEach { batch ->
            rowsAll.upsertAll(batch.map { row("o$it", qtyAll(it), priceAll(cents[it % 1000])) })
        }
    }
    println("%-46s %14s".format("$count rows, one upsert each", perItem(rowByRow)))
    println("%-46s %14s".format("$count rows, upsertAll in batches", perItem(rowsTogether)))
    c.stabilize()
    d.stabilize()
    c.checkConsistency()
    d.checkConsistency()
    println()
    g.checkConsistency()
    println("checked: incremental result matches a full recalculation (total = ${"%.2f".format(g[total])})")
}
