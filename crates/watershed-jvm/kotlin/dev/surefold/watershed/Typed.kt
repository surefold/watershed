// watershed for Kotlin: the typed layer, which is what a program uses.
//
// An expression has a type. `qty * price` is an Expr<Double> if both are;
// `qty + region` (a number and a string) does not compile, and neither does
// setting a calculated node. Nothing is written as text.
//
//     val g = Graph()
//     val qty = g.input("qty", 10.0)                     // Input<Double>
//     val price = g.input("price", 2.5)
//     val total = g.define("total", qty * price * 1.2)   // Node<Double>
//     g[price] = 3.0
//     g.stabilize()
//     val now: Double = g[total]
//
// The types: Double, Long, BigDecimal, Boolean, String and List<String>. A
// whole number written next to a Double or BigDecimal expression is taken
// as one (`price * 12`). A BigDecimal is exact, holds up to 28 digits in the
// engine, and never mixes with a Double by accident.
//
// Underneath, every call goes straight to the engine's C functions through
// the JDK's foreign function API (see Native.java), in tens of nanoseconds.

@file:JvmName("Typed")
@file:Suppress("UNCHECKED_CAST")

package dev.surefold.watershed

import java.math.BigDecimal
import java.math.RoundingMode

/** Something that works out to a [T]: a node, a constant, or a calculation over them. */
open class Expr<T> internal constructor(internal val steps: ByteArray)

/** A node of a graph holding a [T]. */
open class Node<T> internal constructor(internal val graph: Graph, val id: Int) : Expr<T>(Wire.node(id)) {
    override fun equals(other: Any?) = other is Node<*> && other.graph === graph && other.id == id
    override fun hashCode() = id.hashCode()
    override fun toString() = "Node($id)"
}

/** An input: a node that can be set. */
class Input<T> internal constructor(graph: Graph, id: Int) : Node<T>(graph, id)

/** A field or calculated column of a collection's rows, holding a [T] in each row. */
open class Part<T> internal constructor(val name: String) : Expr<T>(Wire.part(name))

/** A field: the part of a row that is given, not calculated. */
class Field<T> internal constructor(name: String) : Part<T>(name)

/** A field's value for a row, made by calling the field: `qty(2.0)`. */
class FieldValue internal constructor(internal val name: String, internal val value: Any)

/** A row to add or change, for [Collection.upsertAll]: `row("o-1", qty(2.0), price(9.99))`. */
class Row internal constructor(internal val key: String, internal val fields: Array<out FieldValue>)

fun row(key: String, vararg fields: FieldValue) = Row(key, fields)

// ------------------------------------------------------------------ values

/** What the engine gave back, as the type the node holds; a failure throws, with why. */
private fun <T> read(held: Any, what: () -> String): T = when (held) {
    is Native.Failed -> throw WatershedException("${what()} could not be calculated: ${held.why}")
    else -> held
} as T

private fun <T> constant(value: Any) = Expr<T>(Wire.constant(value))

private fun <T> unary(op: Byte, a: Expr<*>) = Expr<T>(Wire.unary(op, a.steps))

private fun <T> binary(op: Byte, a: Expr<*>, b: Expr<*>) = Expr<T>(Wire.binary(op, a.steps, b.steps))

/** A constant, for where a value alone would not say which type is meant. */
fun <T : Any> value(value: T): Expr<T> = constant(if (value is Int) value.toLong() else value)

// ------------------------------------------------------------------- graph

/** A graph of inputs and calculated nodes. */
class Graph {
    // Where the graph is in the engine. It is freed when this object can no longer be reached.
    private val address: Long = Native.graph(this)

    private fun <N : Node<*>> mine(node: N): N {
        if (node.graph !== this) throw WatershedException("that node belongs to another graph")
        return node
    }

    fun input(name: String, value: Double): Input<Double> = Input(this, Native.input(address, name, value))
    fun input(name: String, value: Long): Input<Long> = Input(this, Native.input(address, name, value))
    fun input(name: String, value: BigDecimal): Input<BigDecimal> = Input(this, Native.input(address, name, value))
    fun input(name: String, value: Boolean): Input<Boolean> = Input(this, Native.input(address, name, value))
    fun input(name: String, value: String): Input<String> = Input(this, Native.input(address, name, value))

    /** Adds a calculated node. */
    fun <T> define(name: String, expression: Expr<T>): Node<T> = Node(this, Native.define(address, name, expression.steps))

    /** Adds many calculated nodes in one call, in order. */
    fun <T> defineAll(definitions: List<Pair<String, Expr<T>>>): List<Node<T>> =
        Native.defineMany(address, definitions.map { it.first }, definitions.map { it.second.steps })
            .map { Node(this, it) }

    /** Records a new value for an input; it counts from the next `stabilize()`. */
    @JvmName("setNumber")
    operator fun set(input: Input<Double>, value: Double) = Native.setNumber(address, mine(input).id, value)

    @JvmName("setNumberToWhole")
    operator fun set(input: Input<Double>, value: Int) = Native.setNumber(address, mine(input).id, value.toDouble())

    @JvmName("setWhole")
    operator fun set(input: Input<Long>, value: Long) = Native.setWhole(address, mine(input).id, value)

    @JvmName("setWholeToInt")
    operator fun set(input: Input<Long>, value: Int) = Native.setWhole(address, mine(input).id, value.toLong())

    @JvmName("setDecimal")
    operator fun set(input: Input<BigDecimal>, value: BigDecimal) = Native.setDecimal(address, mine(input).id, value)

    @JvmName("setDecimalToWhole")
    operator fun set(input: Input<BigDecimal>, value: Int) = Native.setDecimal(address, mine(input).id, value.toBigDecimal())

    @JvmName("setFlag")
    operator fun set(input: Input<Boolean>, value: Boolean) = Native.setFlag(address, mine(input).id, value)

    @JvmName("setText")
    operator fun set(input: Input<String>, value: String) = Native.setText(address, mine(input).id, value)

    /** Sets many Double inputs in one call: the way to apply a batch of prices. */
    fun setAll(values: Map<Input<Double>, Double>) {
        val nodes = IntArray(values.size)
        val numbers = DoubleArray(values.size)
        var at = 0
        for ((input, value) in values) {
            nodes[at] = mine(input).id
            numbers[at] = value
            at += 1
        }
        Native.setNumbers(address, nodes, numbers)
    }

    /** The node's value as of the last update. A node that could not be calculated throws, with why. */
    operator fun <T> get(node: Node<T>): T =
        read(Native.value(address, mine(node).id)) { Native.name(address, node.id) }

    /** A Double is read as a plain number, with nothing made on the way. */
    @JvmName("getNumber")
    operator fun get(node: Node<Double>): Double = Native.number(address, mine(node).id)

    /** The node's value, or null if it could not be calculated. */
    fun <T> getOrNull(node: Node<T>): T? = when (val held = Native.value(address, mine(node).id)) {
        is Native.Failed -> null
        else -> held as T
    }

    /** Applies every recorded change and recalculates what depends on them, and nothing else. */
    fun stabilize(): Report = Native.stabilize(address)

    /** Why the node has its value, as a tree of what it was worked out from. */
    fun explain(node: Node<*>): String = Native.explain(address, mine(node).id)

    /** The names of the nodes the last update recalculated, in order. */
    fun lastRecalculated(): List<String> = Native.lastRecalculated(address)

    /** How many nodes the graph has had. */
    fun size(): Long = Native.size(address)

    /** Removes a node that nothing uses; its name is then free. */
    fun remove(node: Node<*>) = Native.remove(address, mine(node).id)

    /** Watches a node: in pull mode, it and what it uses are kept up to date. */
    fun watch(node: Node<*>) = Native.watch(address, mine(node).id, true)
    fun unwatch(node: Node<*>) = Native.watch(address, mine(node).id, false)

    /** Pull mode: while on, an update calculates only the watched nodes and what they use. */
    fun calculateOnlyWatched(on: Boolean) = Native.calculateOnlyWatched(address, on)

    /** Checks every node against a calculation from scratch. */
    fun checkConsistency() = Native.checkConsistency(address)

    /** A new, empty keyed table of rows in this graph. */
    fun collection(name: String): Collection = Collection(this, address, name)
}

// -------------------------------------------------------------- collection

/** A keyed table of rows, with fields, calculated columns and totals that update by only the rows that change. */
class Collection internal constructor(private val graph: Graph, graphAddress: Long, name: String) {
    // Holding `graph` keeps the graph alive for as long as this is.
    private val address: Long = Native.collection(this, graphAddress, name)

    private fun <T> field(name: String, kind: Byte): Field<T> {
        Native.field(address, name, kind)
        return Field(name)
    }

    fun number(name: String): Field<Double> = field(name, Wire.NUMBER)
    fun whole(name: String): Field<Long> = field(name, Wire.WHOLE)
    fun decimal(name: String): Field<BigDecimal> = field(name, Wire.DECIMAL)
    fun flag(name: String): Field<Boolean> = field(name, Wire.FLAG)
    fun text(name: String): Field<String> = field(name, Wire.TEXT)

    /** Adds a column, worked out in every row from its fields, other columns and any node. */
    fun <T> column(name: String, expression: Expr<T>): Part<T> {
        Native.column(address, name, expression.steps)
        return Part(name)
    }

    fun <T : Number> sum(name: String, of: Part<T>): Node<T> = Node(graph, Native.total(address, name, Wire.SUM, of.name))
    fun <T : Number> min(name: String, of: Part<T>): Node<T> = Node(graph, Native.total(address, name, Wire.SMALLEST, of.name))
    fun <T : Number> max(name: String, of: Part<T>): Node<T> = Node(graph, Native.total(address, name, Wire.LARGEST, of.name))
    fun count(name: String): Node<Long> = Node(graph, Native.total(address, name, Wire.COUNT, null))

    private fun write(to: Wire, key: String, fields: Array<out FieldValue>) {
        to.put(key).put(fields.size)
        for (field in fields) to.put(field.name).value(field.value)
    }

    /** Adds a row, or changes the fields given of one that is there: `orders.upsert("o-1", qty(2.0), price(9.99))`. */
    fun upsert(key: String, vararg fields: FieldValue) {
        val wire = Wire()
        write(wire, key, fields)
        Native.upsert(address, wire.bytes(), 1)
    }

    /** Adds or changes many rows in one call: `orders.upsertAll(listOf(row("o-1", qty(2.0)), ...))`. */
    fun upsertAll(rows: List<Row>) {
        val wire = Wire()
        for (row in rows) write(wire, row.key, row.fields)
        Native.upsert(address, wire.bytes(), rows.size)
    }

    /** Removes a row. Says whether there was one. */
    fun remove(key: String): Boolean = Native.removeRow(address, key)

    /** A field or column of a row, as of the last update: `orders["o-1", line]`. */
    operator fun <T> get(key: String, part: Part<T>): T =
        read(Native.rowValue(address, key, part.name)) { "$key.${part.name}" }

    /** How many rows there are, counting changes since the last update. */
    fun size(): Long = Native.rows(address)
}

/** A field's value for `upsert`: `qty(2.0)`. */
@JvmName("numberField") operator fun Field<Double>.invoke(value: Double) = FieldValue(name, value)
@JvmName("numberFieldToWhole") operator fun Field<Double>.invoke(value: Int) = FieldValue(name, value.toDouble())
@JvmName("wholeField") operator fun Field<Long>.invoke(value: Long) = FieldValue(name, value)
@JvmName("wholeFieldToInt") operator fun Field<Long>.invoke(value: Int) = FieldValue(name, value.toLong())
@JvmName("decimalField") operator fun Field<BigDecimal>.invoke(value: BigDecimal) = FieldValue(name, value)
@JvmName("decimalFieldToWhole") operator fun Field<BigDecimal>.invoke(value: Int) = FieldValue(name, value.toBigDecimal())
@JvmName("flagField") operator fun Field<Boolean>.invoke(value: Boolean) = FieldValue(name, value)
@JvmName("textField") operator fun Field<String>.invoke(value: String) = FieldValue(name, value)

// ------------------------------------------------------------- expressions

operator fun Expr<Double>.unaryMinus(): Expr<Double> = unary(Wire.NEGATE, this)
@JvmName("negateWhole") operator fun Expr<Long>.unaryMinus(): Expr<Long> = unary(Wire.NEGATE, this)
fun abs(a: Expr<Double>): Expr<Double> = unary(Wire.ABSOLUTE, a)
@JvmName("absWhole") fun abs(a: Expr<Long>): Expr<Long> = unary(Wire.ABSOLUTE, a)

/** Any value as text. */
fun text(a: Expr<*>): Expr<String> = unary(Wire.TO_TEXT, a)

operator fun Expr<Boolean>.not(): Expr<Boolean> = unary(Wire.NOT, this)
infix fun Expr<Boolean>.and(other: Expr<Boolean>): Expr<Boolean> = binary(Wire.AND, this, other)
infix fun Expr<Boolean>.or(other: Expr<Boolean>): Expr<Boolean> = binary(Wire.OR, this, other)

infix fun <T> Expr<T>.eq(other: Expr<T>): Expr<Boolean> = binary(Wire.EQUAL, this, other)
infix fun <T : Any> Expr<T>.eq(other: T): Expr<Boolean> = binary(Wire.EQUAL, this, constant<T>(other))
infix fun <T> Expr<T>.ne(other: Expr<T>): Expr<Boolean> = binary(Wire.NOT_EQUAL, this, other)
infix fun <T : Any> Expr<T>.ne(other: T): Expr<Boolean> = binary(Wire.NOT_EQUAL, this, constant<T>(other))

/** `whenever(condition, then, otherwise)`: only the chosen one is worked out. */
fun <T> whenever(condition: Expr<Boolean>, then: Expr<T>, otherwise: Expr<T>): Expr<T> =
    Expr(Wire.choice(condition.steps, then.steps, otherwise.steps))

fun <T : Any> whenever(condition: Expr<Boolean>, then: T, otherwise: T): Expr<T> =
    whenever(condition, constant<T>(then), constant<T>(otherwise))

/** The option at position `index`, counting from 0; only that one is worked out. */
fun <T> choose(index: Expr<Long>, vararg options: Expr<T>): Expr<T> =
    Expr(Wire.choose(index.steps, options.map { it.steps }))

@JvmName("concatText")
operator fun Expr<String>.plus(other: Expr<String>): Expr<String> = binary(Wire.CONCAT, this, other)
@JvmName("concatTextValue")
operator fun Expr<String>.plus(other: String): Expr<String> = binary(Wire.CONCAT, this, constant<String>(other))

/** A list of these strings, in this order. */
fun texts(vararg items: Expr<String>): Expr<List<String>> =
    Expr(Wire.texts(items.map { it.steps }))

@JvmName("concatTexts")
operator fun Expr<List<String>>.plus(other: Expr<List<String>>): Expr<List<String>> = binary(Wire.CONCAT, this, other)

/** A list's items as one string. */
fun Expr<List<String>>.join(separator: String): Expr<String> = binary(Wire.JOIN, this, constant<String>(separator))

// Arithmetic and ordering, for Double and for Long. They are written out
// rather than generic so that a Double never meets a Long by accident, and
// so that a whole number next to a Double expression (`price * 12`) is a
// Double.


@JvmName("plusNumber") operator fun Expr<Double>.plus(other: Expr<Double>): Expr<Double> = binary(Wire.ADD, this, other)
@JvmName("plusNumberByDouble") operator fun Expr<Double>.plus(other: Double): Expr<Double> = binary(Wire.ADD, this, constant<Double>(other))
@JvmName("doublePlusNumber") operator fun Double.plus(other: Expr<Double>): Expr<Double> = binary(Wire.ADD, constant<Double>(this), other)
@JvmName("plusNumberByInt") operator fun Expr<Double>.plus(other: Int): Expr<Double> = binary(Wire.ADD, this, constant<Double>(other.toDouble()))
@JvmName("intPlusNumber") operator fun Int.plus(other: Expr<Double>): Expr<Double> = binary(Wire.ADD, constant<Double>(this.toDouble()), other)
@JvmName("minusNumber") operator fun Expr<Double>.minus(other: Expr<Double>): Expr<Double> = binary(Wire.SUBTRACT, this, other)
@JvmName("minusNumberByDouble") operator fun Expr<Double>.minus(other: Double): Expr<Double> = binary(Wire.SUBTRACT, this, constant<Double>(other))
@JvmName("doubleMinusNumber") operator fun Double.minus(other: Expr<Double>): Expr<Double> = binary(Wire.SUBTRACT, constant<Double>(this), other)
@JvmName("minusNumberByInt") operator fun Expr<Double>.minus(other: Int): Expr<Double> = binary(Wire.SUBTRACT, this, constant<Double>(other.toDouble()))
@JvmName("intMinusNumber") operator fun Int.minus(other: Expr<Double>): Expr<Double> = binary(Wire.SUBTRACT, constant<Double>(this.toDouble()), other)
@JvmName("timesNumber") operator fun Expr<Double>.times(other: Expr<Double>): Expr<Double> = binary(Wire.MULTIPLY, this, other)
@JvmName("timesNumberByDouble") operator fun Expr<Double>.times(other: Double): Expr<Double> = binary(Wire.MULTIPLY, this, constant<Double>(other))
@JvmName("doubleTimesNumber") operator fun Double.times(other: Expr<Double>): Expr<Double> = binary(Wire.MULTIPLY, constant<Double>(this), other)
@JvmName("timesNumberByInt") operator fun Expr<Double>.times(other: Int): Expr<Double> = binary(Wire.MULTIPLY, this, constant<Double>(other.toDouble()))
@JvmName("intTimesNumber") operator fun Int.times(other: Expr<Double>): Expr<Double> = binary(Wire.MULTIPLY, constant<Double>(this.toDouble()), other)
@JvmName("divNumber") operator fun Expr<Double>.div(other: Expr<Double>): Expr<Double> = binary(Wire.DIVIDE, this, other)
@JvmName("divNumberByDouble") operator fun Expr<Double>.div(other: Double): Expr<Double> = binary(Wire.DIVIDE, this, constant<Double>(other))
@JvmName("doubleDivNumber") operator fun Double.div(other: Expr<Double>): Expr<Double> = binary(Wire.DIVIDE, constant<Double>(this), other)
@JvmName("divNumberByInt") operator fun Expr<Double>.div(other: Int): Expr<Double> = binary(Wire.DIVIDE, this, constant<Double>(other.toDouble()))
@JvmName("intDivNumber") operator fun Int.div(other: Expr<Double>): Expr<Double> = binary(Wire.DIVIDE, constant<Double>(this.toDouble()), other)
@JvmName("leastNumber") infix fun Expr<Double>.least(other: Expr<Double>): Expr<Double> = binary(Wire.LEAST, this, other)
@JvmName("leastNumberByDouble") infix fun Expr<Double>.least(other: Double): Expr<Double> = binary(Wire.LEAST, this, constant<Double>(other))
@JvmName("leastNumberByInt") infix fun Expr<Double>.least(other: Int): Expr<Double> = binary(Wire.LEAST, this, constant<Double>(other.toDouble()))
@JvmName("greatestNumber") infix fun Expr<Double>.greatest(other: Expr<Double>): Expr<Double> = binary(Wire.GREATEST, this, other)
@JvmName("greatestNumberByDouble") infix fun Expr<Double>.greatest(other: Double): Expr<Double> = binary(Wire.GREATEST, this, constant<Double>(other))
@JvmName("greatestNumberByInt") infix fun Expr<Double>.greatest(other: Int): Expr<Double> = binary(Wire.GREATEST, this, constant<Double>(other.toDouble()))
@JvmName("ltNumber") infix fun Expr<Double>.lt(other: Expr<Double>): Expr<Boolean> = binary(Wire.LESS, this, other)
@JvmName("ltNumberByDouble") infix fun Expr<Double>.lt(other: Double): Expr<Boolean> = binary(Wire.LESS, this, constant<Double>(other))
@JvmName("ltNumberByInt") infix fun Expr<Double>.lt(other: Int): Expr<Boolean> = binary(Wire.LESS, this, constant<Double>(other.toDouble()))
@JvmName("leNumber") infix fun Expr<Double>.le(other: Expr<Double>): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, other)
@JvmName("leNumberByDouble") infix fun Expr<Double>.le(other: Double): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, constant<Double>(other))
@JvmName("leNumberByInt") infix fun Expr<Double>.le(other: Int): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, constant<Double>(other.toDouble()))
@JvmName("gtNumber") infix fun Expr<Double>.gt(other: Expr<Double>): Expr<Boolean> = binary(Wire.GREATER, this, other)
@JvmName("gtNumberByDouble") infix fun Expr<Double>.gt(other: Double): Expr<Boolean> = binary(Wire.GREATER, this, constant<Double>(other))
@JvmName("gtNumberByInt") infix fun Expr<Double>.gt(other: Int): Expr<Boolean> = binary(Wire.GREATER, this, constant<Double>(other.toDouble()))
@JvmName("geNumber") infix fun Expr<Double>.ge(other: Expr<Double>): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, other)
@JvmName("geNumberByDouble") infix fun Expr<Double>.ge(other: Double): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, constant<Double>(other))
@JvmName("geNumberByInt") infix fun Expr<Double>.ge(other: Int): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, constant<Double>(other.toDouble()))

@JvmName("plusWhole") operator fun Expr<Long>.plus(other: Expr<Long>): Expr<Long> = binary(Wire.ADD, this, other)
@JvmName("plusWholeByLong") operator fun Expr<Long>.plus(other: Long): Expr<Long> = binary(Wire.ADD, this, constant<Long>(other))
@JvmName("longPlusWhole") operator fun Long.plus(other: Expr<Long>): Expr<Long> = binary(Wire.ADD, constant<Long>(this), other)
@JvmName("plusWholeByInt") operator fun Expr<Long>.plus(other: Int): Expr<Long> = binary(Wire.ADD, this, constant<Long>(other.toLong()))
@JvmName("intPlusWhole") operator fun Int.plus(other: Expr<Long>): Expr<Long> = binary(Wire.ADD, constant<Long>(this.toLong()), other)
@JvmName("minusWhole") operator fun Expr<Long>.minus(other: Expr<Long>): Expr<Long> = binary(Wire.SUBTRACT, this, other)
@JvmName("minusWholeByLong") operator fun Expr<Long>.minus(other: Long): Expr<Long> = binary(Wire.SUBTRACT, this, constant<Long>(other))
@JvmName("longMinusWhole") operator fun Long.minus(other: Expr<Long>): Expr<Long> = binary(Wire.SUBTRACT, constant<Long>(this), other)
@JvmName("minusWholeByInt") operator fun Expr<Long>.minus(other: Int): Expr<Long> = binary(Wire.SUBTRACT, this, constant<Long>(other.toLong()))
@JvmName("intMinusWhole") operator fun Int.minus(other: Expr<Long>): Expr<Long> = binary(Wire.SUBTRACT, constant<Long>(this.toLong()), other)
@JvmName("timesWhole") operator fun Expr<Long>.times(other: Expr<Long>): Expr<Long> = binary(Wire.MULTIPLY, this, other)
@JvmName("timesWholeByLong") operator fun Expr<Long>.times(other: Long): Expr<Long> = binary(Wire.MULTIPLY, this, constant<Long>(other))
@JvmName("longTimesWhole") operator fun Long.times(other: Expr<Long>): Expr<Long> = binary(Wire.MULTIPLY, constant<Long>(this), other)
@JvmName("timesWholeByInt") operator fun Expr<Long>.times(other: Int): Expr<Long> = binary(Wire.MULTIPLY, this, constant<Long>(other.toLong()))
@JvmName("intTimesWhole") operator fun Int.times(other: Expr<Long>): Expr<Long> = binary(Wire.MULTIPLY, constant<Long>(this.toLong()), other)
@JvmName("divWhole") operator fun Expr<Long>.div(other: Expr<Long>): Expr<Long> = binary(Wire.DIVIDE, this, other)
@JvmName("divWholeByLong") operator fun Expr<Long>.div(other: Long): Expr<Long> = binary(Wire.DIVIDE, this, constant<Long>(other))
@JvmName("longDivWhole") operator fun Long.div(other: Expr<Long>): Expr<Long> = binary(Wire.DIVIDE, constant<Long>(this), other)
@JvmName("divWholeByInt") operator fun Expr<Long>.div(other: Int): Expr<Long> = binary(Wire.DIVIDE, this, constant<Long>(other.toLong()))
@JvmName("intDivWhole") operator fun Int.div(other: Expr<Long>): Expr<Long> = binary(Wire.DIVIDE, constant<Long>(this.toLong()), other)
@JvmName("leastWhole") infix fun Expr<Long>.least(other: Expr<Long>): Expr<Long> = binary(Wire.LEAST, this, other)
@JvmName("leastWholeByLong") infix fun Expr<Long>.least(other: Long): Expr<Long> = binary(Wire.LEAST, this, constant<Long>(other))
@JvmName("leastWholeByInt") infix fun Expr<Long>.least(other: Int): Expr<Long> = binary(Wire.LEAST, this, constant<Long>(other.toLong()))
@JvmName("greatestWhole") infix fun Expr<Long>.greatest(other: Expr<Long>): Expr<Long> = binary(Wire.GREATEST, this, other)
@JvmName("greatestWholeByLong") infix fun Expr<Long>.greatest(other: Long): Expr<Long> = binary(Wire.GREATEST, this, constant<Long>(other))
@JvmName("greatestWholeByInt") infix fun Expr<Long>.greatest(other: Int): Expr<Long> = binary(Wire.GREATEST, this, constant<Long>(other.toLong()))
@JvmName("ltWhole") infix fun Expr<Long>.lt(other: Expr<Long>): Expr<Boolean> = binary(Wire.LESS, this, other)
@JvmName("ltWholeByLong") infix fun Expr<Long>.lt(other: Long): Expr<Boolean> = binary(Wire.LESS, this, constant<Long>(other))
@JvmName("ltWholeByInt") infix fun Expr<Long>.lt(other: Int): Expr<Boolean> = binary(Wire.LESS, this, constant<Long>(other.toLong()))
@JvmName("leWhole") infix fun Expr<Long>.le(other: Expr<Long>): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, other)
@JvmName("leWholeByLong") infix fun Expr<Long>.le(other: Long): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, constant<Long>(other))
@JvmName("leWholeByInt") infix fun Expr<Long>.le(other: Int): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, constant<Long>(other.toLong()))
@JvmName("gtWhole") infix fun Expr<Long>.gt(other: Expr<Long>): Expr<Boolean> = binary(Wire.GREATER, this, other)
@JvmName("gtWholeByLong") infix fun Expr<Long>.gt(other: Long): Expr<Boolean> = binary(Wire.GREATER, this, constant<Long>(other))
@JvmName("gtWholeByInt") infix fun Expr<Long>.gt(other: Int): Expr<Boolean> = binary(Wire.GREATER, this, constant<Long>(other.toLong()))
@JvmName("geWhole") infix fun Expr<Long>.ge(other: Expr<Long>): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, other)
@JvmName("geWholeByLong") infix fun Expr<Long>.ge(other: Long): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, constant<Long>(other))
@JvmName("geWholeByInt") infix fun Expr<Long>.ge(other: Int): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, constant<Long>(other.toLong()))

// Decimals: exact, for money and anything that must reconcile to the last
// digit. Adding, subtracting and multiplying are exact within 28 digits;
// dividing rounds at the last digit; `round` puts a value on the number of
// places you want.

@JvmName("negateDecimal") operator fun Expr<BigDecimal>.unaryMinus(): Expr<BigDecimal> = unary(Wire.NEGATE, this)
@JvmName("absDecimal") fun abs(a: Expr<BigDecimal>): Expr<BigDecimal> = unary(Wire.ABSOLUTE, a)

/** To this many decimal places (0 to 28). A tie goes to the even neighbour unless another way is given. */
fun Expr<BigDecimal>.round(places: Int, rounding: RoundingMode = RoundingMode.HALF_EVEN): Expr<BigDecimal> =
    round(constant<Long>(places.toLong()), rounding)

fun Expr<BigDecimal>.round(places: Expr<Long>, rounding: RoundingMode = RoundingMode.HALF_EVEN): Expr<BigDecimal> {
    val op: Byte = when (rounding) {
        RoundingMode.HALF_EVEN -> Wire.ROUND_HALF_EVEN
        RoundingMode.HALF_UP -> Wire.ROUND_HALF_UP
        RoundingMode.DOWN -> Wire.ROUND_DOWN
        RoundingMode.UP -> Wire.ROUND_UP
        else -> throw IllegalArgumentException("rounding is HALF_EVEN, HALF_UP, DOWN or UP, not $rounding")
    }
    return binary(op, this, places)
}

/** A whole number as the same decimal, exactly. */
fun Expr<Long>.toDecimal(): Expr<BigDecimal> = unary(Wire.TO_DECIMAL, this)

/** A decimal as the nearest Double, for where exactness is not needed. */
fun Expr<BigDecimal>.toDouble(): Expr<Double> = unary(Wire.TO_NUMBER, this)

@JvmName("plusDecimal") operator fun Expr<BigDecimal>.plus(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.ADD, this, other)
@JvmName("plusDecimalByBigDecimal") operator fun Expr<BigDecimal>.plus(other: BigDecimal): Expr<BigDecimal> = binary(Wire.ADD, this, constant<BigDecimal>(other))
@JvmName("bigdecimalPlusDecimal") operator fun BigDecimal.plus(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.ADD, constant<BigDecimal>(this), other)
@JvmName("plusDecimalByInt") operator fun Expr<BigDecimal>.plus(other: Int): Expr<BigDecimal> = binary(Wire.ADD, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("intPlusDecimal") operator fun Int.plus(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.ADD, constant<BigDecimal>(this.toBigDecimal()), other)
@JvmName("minusDecimal") operator fun Expr<BigDecimal>.minus(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.SUBTRACT, this, other)
@JvmName("minusDecimalByBigDecimal") operator fun Expr<BigDecimal>.minus(other: BigDecimal): Expr<BigDecimal> = binary(Wire.SUBTRACT, this, constant<BigDecimal>(other))
@JvmName("bigdecimalMinusDecimal") operator fun BigDecimal.minus(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.SUBTRACT, constant<BigDecimal>(this), other)
@JvmName("minusDecimalByInt") operator fun Expr<BigDecimal>.minus(other: Int): Expr<BigDecimal> = binary(Wire.SUBTRACT, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("intMinusDecimal") operator fun Int.minus(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.SUBTRACT, constant<BigDecimal>(this.toBigDecimal()), other)
@JvmName("timesDecimal") operator fun Expr<BigDecimal>.times(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.MULTIPLY, this, other)
@JvmName("timesDecimalByBigDecimal") operator fun Expr<BigDecimal>.times(other: BigDecimal): Expr<BigDecimal> = binary(Wire.MULTIPLY, this, constant<BigDecimal>(other))
@JvmName("bigdecimalTimesDecimal") operator fun BigDecimal.times(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.MULTIPLY, constant<BigDecimal>(this), other)
@JvmName("timesDecimalByInt") operator fun Expr<BigDecimal>.times(other: Int): Expr<BigDecimal> = binary(Wire.MULTIPLY, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("intTimesDecimal") operator fun Int.times(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.MULTIPLY, constant<BigDecimal>(this.toBigDecimal()), other)
@JvmName("divDecimal") operator fun Expr<BigDecimal>.div(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.DIVIDE, this, other)
@JvmName("divDecimalByBigDecimal") operator fun Expr<BigDecimal>.div(other: BigDecimal): Expr<BigDecimal> = binary(Wire.DIVIDE, this, constant<BigDecimal>(other))
@JvmName("bigdecimalDivDecimal") operator fun BigDecimal.div(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.DIVIDE, constant<BigDecimal>(this), other)
@JvmName("divDecimalByInt") operator fun Expr<BigDecimal>.div(other: Int): Expr<BigDecimal> = binary(Wire.DIVIDE, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("intDivDecimal") operator fun Int.div(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.DIVIDE, constant<BigDecimal>(this.toBigDecimal()), other)
@JvmName("leastDecimal") infix fun Expr<BigDecimal>.least(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.LEAST, this, other)
@JvmName("leastDecimalByBigDecimal") infix fun Expr<BigDecimal>.least(other: BigDecimal): Expr<BigDecimal> = binary(Wire.LEAST, this, constant<BigDecimal>(other))
@JvmName("leastDecimalByInt") infix fun Expr<BigDecimal>.least(other: Int): Expr<BigDecimal> = binary(Wire.LEAST, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("greatestDecimal") infix fun Expr<BigDecimal>.greatest(other: Expr<BigDecimal>): Expr<BigDecimal> = binary(Wire.GREATEST, this, other)
@JvmName("greatestDecimalByBigDecimal") infix fun Expr<BigDecimal>.greatest(other: BigDecimal): Expr<BigDecimal> = binary(Wire.GREATEST, this, constant<BigDecimal>(other))
@JvmName("greatestDecimalByInt") infix fun Expr<BigDecimal>.greatest(other: Int): Expr<BigDecimal> = binary(Wire.GREATEST, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("ltDecimal") infix fun Expr<BigDecimal>.lt(other: Expr<BigDecimal>): Expr<Boolean> = binary(Wire.LESS, this, other)
@JvmName("ltDecimalByBigDecimal") infix fun Expr<BigDecimal>.lt(other: BigDecimal): Expr<Boolean> = binary(Wire.LESS, this, constant<BigDecimal>(other))
@JvmName("ltDecimalByInt") infix fun Expr<BigDecimal>.lt(other: Int): Expr<Boolean> = binary(Wire.LESS, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("leDecimal") infix fun Expr<BigDecimal>.le(other: Expr<BigDecimal>): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, other)
@JvmName("leDecimalByBigDecimal") infix fun Expr<BigDecimal>.le(other: BigDecimal): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, constant<BigDecimal>(other))
@JvmName("leDecimalByInt") infix fun Expr<BigDecimal>.le(other: Int): Expr<Boolean> = binary(Wire.LESS_OR_EQUAL, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("gtDecimal") infix fun Expr<BigDecimal>.gt(other: Expr<BigDecimal>): Expr<Boolean> = binary(Wire.GREATER, this, other)
@JvmName("gtDecimalByBigDecimal") infix fun Expr<BigDecimal>.gt(other: BigDecimal): Expr<Boolean> = binary(Wire.GREATER, this, constant<BigDecimal>(other))
@JvmName("gtDecimalByInt") infix fun Expr<BigDecimal>.gt(other: Int): Expr<Boolean> = binary(Wire.GREATER, this, constant<BigDecimal>(other.toBigDecimal()))
@JvmName("geDecimal") infix fun Expr<BigDecimal>.ge(other: Expr<BigDecimal>): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, other)
@JvmName("geDecimalByBigDecimal") infix fun Expr<BigDecimal>.ge(other: BigDecimal): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, constant<BigDecimal>(other))
@JvmName("geDecimalByInt") infix fun Expr<BigDecimal>.ge(other: Int): Expr<Boolean> = binary(Wire.GREATER_OR_EQUAL, this, constant<BigDecimal>(other.toBigDecimal()))
