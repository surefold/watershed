# watershed from Kotlin

A small Kotlin program that uses the engine through `crates/watershed-jvm`.

```bash
cd examples/kotlin
gradle run      # builds the Rust library, writes its Kotlin side, runs src/main/kotlin/Main.kt
gradle test     # the same, then runs src/test/kotlin/WatershedTest.kt
```

It needs Rust 1.88 or later (`cargo`), a JDK from 21 up, and Gradle. The first
run downloads UniFFI and compiles it, which takes a minute; after that only
what changed is rebuilt.

## What it looks like

```kotlin
val g = Graph()
val qty = g.input("qty", Value.Number(10.0))
val price = g.input("price", Value.Number(2.5))
val total = g.define("total", "qty * price * 1.2")   // a formula as text

g.set(price, Value.Number(3.0))
val report = g.stabilize()       // report.recalculated == 1
g.get(total)                     // Value.Number(36.0)
g.explain(total)                 // why, as a tree

val orders = g.collection("orders")
orders.field("qty", Kind.NUMBER)
orders.field("price", Kind.NUMBER)
orders.column("line", "qty * price")
val revenue = orders.sum("revenue", "line")
orders.upsert("o-1", mapOf("qty" to Value.Number(2.0), "price" to Value.Number(9.99)))
g.stabilize()
```

A node is a `UInt`. A value is a `Value`: `Number`, `Whole`, `Flag`, `Text`,
`Texts` (a list of strings) or `Failed` (the node could not be calculated,
and why). Anything the engine refuses is a `WatershedException` with its
reason.

## How it fits together

- `crates/watershed-jvm` is a Rust library that wraps the engine in a few
  plain types. [UniFFI](https://mozilla.github.io/uniffi-rs/) writes the
  Kotlin classes that call it, through [JNA](https://github.com/java-native-access/jna).
- `build.gradle.kts` has two tasks ahead of compiling Kotlin: `buildNative`
  (`cargo build --release` in that crate) and `generateBindings` (the
  Kotlin, into `build/generated/uniffi`). `run` and `test` tell the JVM
  where the library is with `-Djna.library.path`.
- Nothing is published or installed. To use it in another project today,
  copy those two tasks, or copy the generated `watershed_jvm.kt` and the
  built library (`libwatershed_jvm.dylib`, `.so` or `.dll`) into it.

## What is not here yet

- Calls go one at a time, each through JNA, so adding 100,000 rows from
  Kotlin is slower than from Rust. A call that takes many rows at once
  would fix that.
- Groupings, history (`explain` as of an earlier update) and expressions
  built in Kotlin rather than written as text.
- A jar with the library inside for each platform.
