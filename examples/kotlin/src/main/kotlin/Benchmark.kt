import dev.surefold.watershed.*
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
    val micros = d.inWholeNanoseconds / 1000.0
    return if (micros < 1000) "%.1f µs".format(micros) else "%.1f ms".format(micros / 1000)
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
    val read = median(2_000) { g[total] }
    // The same update with nothing to recalculate: what one set and one
    // stabilize cost to call at all.
    val quiet = Graph()
    val lone = quiet.input("lone", 1.0)
    var tick = 0.0
    repeat(20_000) { quiet[lone] = tick++; quiet.stabilize() }
    val call = median(20_000) { quiet[lone] = tick++; quiet.stabilize() }

    println("%-46s %12s".format("median of many runs, from Kotlin", "time"))
    println("%-46s %12s".format("incremental: one price changes", show(one)))
    println("%-46s %12s".format("incremental: 1% of prices change together", show(some)))
    println("%-46s %12s".format("incremental: 10% of prices change together", show(many)))
    println("%-46s %12s".format("read one value", show(read)))
    println("%-46s %12s".format("one set and one update that changes nothing", show(call)))
    println()
    g.checkConsistency()
    println("checked: incremental result matches a full recalculation (total = ${"%.2f".format(g[total])})")
}
