# watershed from Kotlin

A small Kotlin program that uses the engine through `crates/watershed-jvm`.

```bash
cd examples/kotlin
gradle run      # builds the Rust library, writes its Kotlin side, runs src/main/kotlin/Main.kt
gradle test     # the same, then runs src/test/kotlin/WatershedTest.kt
gradle benchmark  # times the engine's benchmark graph from Kotlin
```

It needs Rust 1.88 or later (`cargo`), a JDK from 21 up, and Gradle. The first
run downloads UniFFI and compiles it, which takes a minute; after that only
what changed is rebuilt.

## What it looks like

```kotlin
val g = Graph()
val qty = g.input("qty", 10.0)                     // Input<Double>
val price = g.input("price", 2.5)
val total = g.define("total", qty * price * 1.2)   // Node<Double>

g[price] = 3.0
val report = g.stabilize()       // report.recalculated == 1
val now: Double = g[total]       // 36.0
g.explain(total)                 // why, as a tree

val orders = g.collection("orders")
val units = orders.number("units")                 // Field<Double>
val unitPrice = orders.number("unit price")
val line = orders.column("line", units * unitPrice)
val revenue = orders.sum("revenue", line)          // Node<Double>
orders.upsert("o-1", units(2), unitPrice(9.99))
g.stabilize()
orders["o-1", line]              // 19.98
```

Expressions are typed, as in Rust, and nothing is written as text:

- `qty + region` (a number plus a string) does not compile.
- `g[total] = 5.0` does not compile: `total` is a `Node`, not an `Input`.
- `g[price] = "three"` does not compile: `price` holds a `Double`.

The types are `Double`, `Long`, `BigDecimal`, `Boolean`, `String` and
`List<String>`. A whole number written next to a `Double` or `BigDecimal`
expression is taken as one (`price * 12`).

A `BigDecimal` is exact, for money: `net * vat` keeps every digit,
`.round(2)` puts it on two places (a tie going to the even neighbour, or
`.round(2, RoundingMode.HALF_UP)`), and it does not mix with a `Double`
unless you say `.toDouble()`. The engine holds 28 significant digits; a
`BigDecimal` with more is refused, not rounded. Kotlin's `<` and `==` cannot build an expression, so
comparisons are words: `a lt b`, `le`, `gt`, `ge`, `eq`, `ne`, with `and`,
`or`, `!`, `whenever(condition, then, otherwise)`, `choose`, `text`, `texts`,
`join`, `abs`, `least` and `greatest`.

A node that could not be calculated (a division by zero, say) makes `g[node]`
throw a `WatershedException` with the reason; `g.getOrNull(node)` gives
`null` instead. Anything the engine refuses is a `WatershedException` too.

## How fast

`gradle benchmark` builds the engine's benchmark graph (about a million
nodes) from Kotlin and times the same updates as
`cargo run --release --example benchmark`. Side by side on one machine, run
one after the other while it was busy with other work, so read them as a
comparison and not as absolute numbers:

| | from Rust | from Kotlin |
|---|---|---|
| build the graph | 1.2 s | 3.6 s (in calls of 10,000 nodes) |
| one price changes | 240 µs | 300 µs |
| 1% of prices change together | 9.7 ms | 13.7 ms (one `setAll`) |
| read one `Double` | | 7 µs |
| one set and one update that changes nothing | | 26 µs |

The engine does the same work either way; what Kotlin adds is the cost of
each call into it, about 10 µs through JNA. So:

- An update is a set and a `stabilize()`: two calls, which is small beside
  anything but the smallest update.
- Thousands of calls add up. Building the million nodes one `define` at a
  time took 63 s; `defineAll` in batches of 10,000 took 3.6 s. `setAll`
  does the same for a batch of prices.
- A `Double` is set and read as a plain number, which is the fast path;
  other types go as a `Value`, which costs a few times more per call.

If calls of 10 µs are too slow for what you are doing, the bindings want
JNI or the JDK's foreign function API underneath instead of JNA; the typed
layer on top would not change.

## How it fits together

- `crates/watershed-jvm` is a Rust library that wraps the engine in a few
  plain types. [UniFFI](https://mozilla.github.io/uniffi-rs/) writes the
  Kotlin classes that call it, through [JNA](https://github.com/java-native-access/jna).
- `crates/watershed-jvm/kotlin/` is the typed layer, written by hand over
  those generated calls: `Expr<T>`, `Node<T>`, `Input<T>`, `Field<T>` and
  the operators. An expression crosses into the engine as a list of steps
  (`qty`, `price`, multiply), not as text, and the engine checks its types
  again when the node is made.
- `build.gradle.kts` has two tasks ahead of compiling Kotlin: `buildNative`
  (`cargo build --release` in that crate) and `generateBindings` (the
  Kotlin, into `build/generated/uniffi`). `run` and `test` tell the JVM
  where the library is with `-Djna.library.path`.
- Nothing is published or installed. To use it in another project today,
  copy those two tasks, or copy the generated `watershed_jvm.kt`, the
  typed layer's `Typed.kt` and the built library (`libwatershed_jvm.dylib`, `.so` or `.dll`) into it.

## What is not here yet

- Rows of a collection still go one `upsert` at a time; nodes and prices
  have calls that take many (`defineAll`, `setAll`).
- Groupings and history (`explain` as of an earlier update).
- A jar with the library inside for each platform.
