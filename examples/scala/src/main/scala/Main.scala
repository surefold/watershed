import watershed.*

@main def run(): Unit =
  // ---------------------------------------------------------------- a graph
  val g = Graph()
  val qty = g.input("qty", 10.0)            // Input[Double]
  val price = g.input("price", 2.5)
  val taxRate = g.input("tax rate", 0.2)
  val region = g.input("region", "EU")      // Input[String]

  val subtotal = g.define("subtotal", qty * price)            // Node[Double]
  val total = g.define("total", subtotal * (1 + taxRate))
  val shipping = g.define("shipping", whenever(qty > 20, 0.0, 4.99))
  val label = g.define("label", region ++ ": " ++ text(total))  // Node[String]

  // These do not compile, as they would not in Rust:
  //   g.define("wrong", qty + region)     a number plus a string
  //   g(total) = 5.0                      total is calculated, not an input
  //   g(price) = "three"                  price holds a Double

  val first: Double = g(total)
  println(s"total = $first, label = ${g(label)}")

  // Change one input: only what depends on it is recalculated.
  g(price) = 3.0
  g.stabilize()
  println(s"price 2.5 -> 3.0 recalculated ${g.lastRecalculated.mkString(", ")}, not shipping")
  println(s"total = ${g(total)}")
  println()
  println(g.explain(total))

  // An update stops where a value comes out the same: qty 10 -> 12 leaves
  // shipping at 4.99, so nothing above shipping would be told.
  g(qty) = 12
  var report = g.stabilize()
  println(s"qty 10 -> 12: ${report.recalculated} recalculated, ${report.changed} changed")
  println(s"shipping = ${g(shipping)}, subtotal = ${g(subtotal)}")
  println()

  // A calculation that fails is a value the node holds, with its reason.
  val none = g.input("none", 0L)
  val share = g.define("share", 100L / none)
  println(s"share = ${g.get(share)}")
  try g(share)
  catch case e: WatershedException => println(s"refused: ${e.getMessage}")
  println()

  // ------------------------------------------------------------- decimals
  // A BigDecimal is exact: money that has to reconcile to the last digit.
  val net = g.input("net", BigDecimal("19.99"))
  val vat = g.input("vat", BigDecimal("0.20"))
  val tax = g.define("tax", (net * vat).round(2))             // Node[BigDecimal]
  val gross = g.define("gross", net + tax)
  println(s"net ${g(net)} + tax ${g(tax)} = ${g(gross)}")
  // g.define("mixed", net * price) does not compile: a BigDecimal and a Double.
  println()

  // ------------------------------------------------------------ a collection
  // A keyed table of rows. Every cell is a node, and totals update by only
  // the rows that change.
  val fx = g.input("fx", 1.25)
  val orders = g.collection("orders")
  val units = orders.number("units")              // Field[Double]
  val unitPrice = orders.number("unit price")
  val line = orders.column("line", units * unitPrice * fx)
  val revenue = orders.sum("revenue", line)       // Node[Double]
  val largest = orders.max("largest", line)
  val count = orders.count("orders")              // Node[Long]

  val rows = 100_000
  for i <- 0 until rows do orders.upsert(s"o$i", units := i % 100, unitPrice := 1.5)
  g.stabilize()
  println(s"$rows rows: ${g.size} nodes, revenue = ${g(revenue)}, rows = ${g(count)}")

  orders.upsert("o42", units := 1000)
  report = g.stabilize()
  println(s"one row changed: ${report.recalculated} nodes recalculated (its line and two totals), largest = ${g(largest)}")

  g(fx) = 1.3
  report = g.stabilize()
  println(s"the exchange rate changed: ${report.recalculated} nodes recalculated (every line)")
  println(s"row o42's line = ${orders("o42", line)}")

  g.checkConsistency()
  println("checked: every node matches a calculation from scratch")
