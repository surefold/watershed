# watershed from Kotlin

A small Kotlin program that uses the engine through `crates/watershed-jvm`.

```bash
cd examples/kotlin
gradle run      # builds the Rust library, writes its Kotlin side, runs src/main/kotlin/Main.kt
gradle test     # the same, then runs src/test/kotlin/WatershedTest.kt
gradle benchmark  # times the engine's benchmark graph from Kotlin
```

It needs Rust (`cargo`), a JDK from 22 up, and Gradle.

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
`cargo run --release --example benchmark`. Side by side on one machine, one
after the other:

| | from Rust | from Kotlin |
|---|---|---|
| one price changes | 95 µs | 93 µs |
| 1% of prices change together | 4.2 ms | 4.5 ms (one `setAll`) |
| 10% of prices change together | 8.9 ms | 7.0 ms |
| build the graph | 0.46 s | 0.79 s (in calls of 10,000 nodes) |

(The machine was doing other work, so rows move by a tenth or so from run
to run; Rust and Kotlin are level.)

What one call costs from Kotlin:

| | set | read |
|---|---|---|
| a `Double` | 125 ns | 42 ns |
| a `Long` | 41 ns | 41 ns |
| a `BigDecimal` | 83 ns | 250 ns |
| a `String` | 208 ns | 125 ns |

An update with nothing to do is 42 ns. (The clock here ticks every 41 ns,
so the smallest of these are "one tick".) Adding a node is about 0.8 µs one
`define` at a time and 0.7 µs with `defineAll`; adding a row of two fields
and a column is about 1.2 µs with `upsert` and 1.1 µs with `upsertAll`. So
the calls that take many at once are a convenience now, not a necessity.

It was not always so. The first bindings were generated (UniFFI) and went
through JNA, at about 10 µs a call: a set and an update that changed
nothing took 26 µs, and a million nodes took 63 s to build one `define` at
a time. Every call now goes straight to a C function in the engine through
the JDK's foreign function API.

## How it fits together

- `crates/watershed-jvm` is the engine as a C library: about thirty plain
  functions (`src/ffi.rs`). Numbers cross in registers; a name, an
  expression or a batch of rows crosses as bytes the JVM wrote.
- `crates/watershed-jvm/java/` is one small Java class, `Native`, that
  calls those functions through `java.lang.foreign` (so: JDK 22 or later,
  and `--enable-native-access`), and `Wire`, which writes the bytes.
- `crates/watershed-jvm/kotlin/Typed.kt` is the typed layer a program
  uses: `Graph`, `Collection`, `Expr<T>`, `Node<T>`, `Input<T>`,
  `Field<T>` and the operators. Scala has its own beside it.
- `build.gradle.kts` runs `cargo build --release` and tells the JVM where
  the library is with `-Dwatershed.library.path`.
- Nothing is generated, published or installed, and there are no
  dependencies beyond the JDK. To use it in another project today, copy the
  `java/` and `kotlin/` sources and the built library
  (`libwatershed_jvm.dylib`, `.so` or `.dll`).

## What is not here yet

- Groupings and history (`explain` as of an earlier update).
- A jar with the library inside for each platform.
- `Collection` has the name of Kotlin's own `Collection`, which wins when
  you write the type out: say `dev.surefold.watershed.Collection`, or let
  it be inferred.
