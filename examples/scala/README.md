# watershed from Scala 3

A small Scala 3 program that uses the engine through `crates/watershed-jvm`.

```bash
cd examples/scala
sbt run      # builds the Rust library and its JVM bindings, runs src/main/scala/Main.scala
sbt test     # the same, then runs src/test/scala/WatershedTest.scala
```

It needs Rust 1.88 or later (`cargo`), a JDK from 21 up, Gradle and sbt.
Gradle is there because the bindings are generated as Kotlin and compiled
in `../kotlin`; sbt runs that and takes the classes.

## What it looks like

```scala
import watershed.*

val g = Graph()
val qty = g.input("qty", 10.0)                     // Input[Double]
val price = g.input("price", 2.5)
val total = g.define("total", qty * price * 1.2)   // Node[Double]

g(price) = 3.0
val report = g.stabilize()       // report.recalculated == 1
val now: Double = g(total)       // 36.0
g.explain(total)                 // why, as a tree

val orders = g.collection("orders")
val units = orders.number("units")                 // Field[Double]
val unitPrice = orders.number("unit price")
val line = orders.column("line", units * unitPrice)
val revenue = orders.sum("revenue", line)          // Node[Double]
orders.upsert("o-1", units := 2, unitPrice := 9.99)
g.stabilize()
orders("o-1", line)              // 19.98
```

Expressions are typed, as in Rust, and nothing is written as text. The
tests check that each of these fails to compile:

- `qty + region`: a number plus a string
- `qty * n`: a `Double` times a `Long`
- `net * qty`: a `BigDecimal` times a `Double`
- `qty && region`: logic on what is not a `Boolean`
- `g(total) = 5.0`: `total` is a `Node`, not an `Input`
- `g(qty) = "three"`: `qty` holds a `Double`

The types are `Double`, `Long`, `BigDecimal`, `Boolean`, `String` and
`List[String]`. A whole number written beside a number expression is taken
as that kind of number (`price * 12`, `1 + taxRate`).

Scala lets more of this be ordinary operators than Kotlin does: `<`, `<=`,
`>`, `>=`, `&&`, `||` and `!` build expressions. Only equality needs its own
spelling, `===` and `=!=`, because `==` must give a `Boolean`. Text and
lists join with `++`. The conditional is `whenever(condition, then,
otherwise)`.

A `BigDecimal` is exact, for money: `(net * vat).round(2)` puts a value on
two places, a tie going to the even neighbour, or
`.round(2, RoundingMode.HALF_UP)`. It does not mix with a `Double` unless
you say `.toDouble`. The engine holds 28 significant digits; a `BigDecimal`
with more is refused, not rounded.

A node that could not be calculated (a division by zero, say) makes
`g(node)` throw a `WatershedException` with the reason; `g.get(node)` gives
`None` instead.

## How it fits together

- `crates/watershed-jvm` is the Rust library, with its JVM side generated
  as Kotlin by [UniFFI](https://mozilla.github.io/uniffi-rs/).
- `crates/watershed-jvm/scala/watershed/Watershed.scala` is the typed layer
  for Scala, written by hand over those generated classes: `Expr[T]`,
  `Node[T]`, `Input[T]`, `Field[T]`, the operators, and the type classes
  (`Holds`, `Number`, `Ordered`) that decide what compiles.
- `build.sbt` runs `gradle jar` in `../kotlin` for the bindings, adds JNA
  and the Kotlin standard library they need, and points the JVM at the
  native library with `-Djna.library.path`.

## What is not here yet

- The same as for Kotlin (`../kotlin/README.md`): each call into the engine
  costs about 10 µs through JNA, so use `defineAll` and `setAll` for
  thousands of nodes or prices; rows still go one `upsert` at a time; no
  groupings or history; nothing packaged as a jar.
- The build needs Gradle as well as sbt. Bindings generated as Java, or a
  published jar of the Kotlin ones, would remove that.
