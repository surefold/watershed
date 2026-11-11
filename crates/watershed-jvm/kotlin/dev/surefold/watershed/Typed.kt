// The typed layer over the generated bindings: what a Kotlin program uses.
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
// The types: Double, Long, Boolean, String and List<String>. A whole number
// written next to a Double expression is taken as a Double (`price * 12`).

@file:JvmName("Typed")
@file:Suppress("UNCHECKED_CAST")

package dev.surefold.watershed

/** Something that works out to a [T]: a node, a constant, or a calculation over them. */
open class Expr<T> internal constructor(internal val ops: List<Op>)

/** A node of a graph holding a [T]. */
open class Node<T> internal constructor(val id: UInt) : Expr<T>(listOf(Op.Node(id))) {
    override fun equals(other: Any?) = other is Node<*> && other.id == id
    override fun hashCode() = id.hashCode()
    override fun toString() = "Node($id)"
}

/** An input: a node that can be set. */
class Input<T> internal constructor(id: UInt) : Node<T>(id)

/** A field or calculated column of a collection's rows, holding a [T] in each row. */
open class Part<T> internal constructor(val name: String) : Expr<T>(listOf(Op.Part(name)))

/** A field: the part of a row that is given, not calculated. */
class Field<T> internal constructor(name: String) : Part<T>(name)

/** A field's value for a row, made by calling the field: `qty(2.0)`. */
class FieldValue internal constructor(internal val name: String, internal val value: Value)

// ------------------------------------------------------------------ values

private fun valueOf(value: Any?): Value = when (value) {
    is Double -> Value.Number(value)
    is Long -> Value.Whole(value)
    is Int -> Value.Whole(value.toLong())
    is Boolean -> Value.Flag(value)
    is String -> Value.Text(value)
    else -> throw IllegalArgumentException("a value is a Double, a Long, a Boolean or a String, not $value")
}

private fun <T> read(value: Value, what: String): T = when (value) {
    is Value.Number -> value.value
    is Value.Whole -> value.value
    is Value.Flag -> value.value
    is Value.Text -> value.value
    is Value.Texts -> value.items
    is Value.Failed -> throw WatershedException.Refused("$what could not be calculated: ${value.message}")
} as T

private fun <T> constant(value: Any?) = Expr<T>(listOf(Op.Constant(valueOf(value))))

private fun <T> unary(op: UnaryOp, a: Expr<*>) = Expr<T>(a.ops + Op.Unary(op))

private fun <T> binary(op: BinaryOp, a: Expr<*>, b: Expr<*>) = Expr<T>(a.ops + b.ops + Op.Binary(op))

/** A constant, for where a value alone would not say which type is meant. */
fun <T : Any> value(value: T): Expr<T> = constant(value)

// ------------------------------------------------------------------- graph

fun Graph.input(name: String, value: Double): Input<Double> = Input(inputValue(name, Value.Number(value)))
fun Graph.input(name: String, value: Long): Input<Long> = Input(inputValue(name, Value.Whole(value)))
fun Graph.input(name: String, value: Boolean): Input<Boolean> = Input(inputValue(name, Value.Flag(value)))
fun Graph.input(name: String, value: String): Input<String> = Input(inputValue(name, Value.Text(value)))

/** Adds a calculated node. */
fun <T> Graph.define(name: String, expression: Expr<T>): Node<T> = Node(defineExpression(name, expression.ops))

/** Records a new value for an input; it counts from the next `stabilize()`. */
operator fun <T : Any> Graph.set(input: Input<T>, value: T) = setValue(input.id, valueOf(value))

@JvmName("setNumberToWhole")
operator fun Graph.set(input: Input<Double>, value: Int) = setValue(input.id, Value.Number(value.toDouble()))

/** The node's value as of the last update. A node that could not be calculated throws, with why. */
operator fun <T> Graph.get(node: Node<T>): T = read(value(node.id), name(node.id))

/** The node's value, or null if it could not be calculated. */
fun <T> Graph.getOrNull(node: Node<T>): T? = when (val held = value(node.id)) {
    is Value.Failed -> null
    else -> read(held, name(node.id))
}

fun Graph.explain(node: Node<*>): String = explain(node.id)
fun Graph.watch(node: Node<*>) = watch(node.id)
fun Graph.unwatch(node: Node<*>) = unwatch(node.id)
fun Graph.remove(node: Node<*>) = remove(node.id)

// -------------------------------------------------------------- collection

@JvmName("numberField") fun Collection.number(name: String): Field<Double> = field(name, Kind.NUMBER)
@JvmName("wholeField") fun Collection.whole(name: String): Field<Long> = field(name, Kind.WHOLE)
@JvmName("flagField") fun Collection.flag(name: String): Field<Boolean> = field(name, Kind.FLAG)
@JvmName("textField") fun Collection.text(name: String): Field<String> = field(name, Kind.TEXT)

private fun <T> Collection.field(name: String, kind: Kind): Field<T> {
    addField(name, kind)
    return Field(name)
}

/** Adds a column, worked out in every row from its fields, other columns and any node. */
fun <T> Collection.column(name: String, expression: Expr<T>): Part<T> {
    addColumn(name, expression.ops)
    return Part(name)
}

fun <T : Number> Collection.sum(name: String, of: Part<T>): Node<T> = Node(addSum(name, of.name))
fun <T : Number> Collection.min(name: String, of: Part<T>): Node<T> = Node(addMin(name, of.name))
fun <T : Number> Collection.max(name: String, of: Part<T>): Node<T> = Node(addMax(name, of.name))
fun Collection.count(name: String): Node<Long> = Node(addCount(name))

/** A field's value for `upsert`: `qty(2.0)`. */
operator fun <T : Any> Field<T>.invoke(value: T) = FieldValue(name, valueOf(value))

@JvmName("numberFieldToWhole")
operator fun Field<Double>.invoke(value: Int) = FieldValue(name, Value.Number(value.toDouble()))

/** Adds a row, or changes the fields given of one that is there: `orders.upsert("o-1", qty(2.0), price(9.99))`. */
fun Collection.upsert(key: String, vararg fields: FieldValue) =
    upsertValues(key, fields.associate { it.name to it.value })

/** A field or column of a row, as of the last update: `orders["o-1", line]`. */
operator fun <T> Collection.get(key: String, part: Part<T>): T = read(value(key, part.name), "$key.${part.name}")

// ------------------------------------------------------------- expressions

operator fun Expr<Double>.unaryMinus(): Expr<Double> = unary(UnaryOp.NEGATE, this)
@JvmName("negateWhole") operator fun Expr<Long>.unaryMinus(): Expr<Long> = unary(UnaryOp.NEGATE, this)
fun abs(a: Expr<Double>): Expr<Double> = unary(UnaryOp.ABSOLUTE, a)
@JvmName("absWhole") fun abs(a: Expr<Long>): Expr<Long> = unary(UnaryOp.ABSOLUTE, a)

/** Any value as text. */
fun text(a: Expr<*>): Expr<String> = unary(UnaryOp.TO_TEXT, a)

operator fun Expr<Boolean>.not(): Expr<Boolean> = unary(UnaryOp.NOT, this)
infix fun Expr<Boolean>.and(other: Expr<Boolean>): Expr<Boolean> = binary(BinaryOp.AND, this, other)
infix fun Expr<Boolean>.or(other: Expr<Boolean>): Expr<Boolean> = binary(BinaryOp.OR, this, other)

infix fun <T> Expr<T>.eq(other: Expr<T>): Expr<Boolean> = binary(BinaryOp.EQUAL, this, other)
infix fun <T : Any> Expr<T>.eq(other: T): Expr<Boolean> = binary(BinaryOp.EQUAL, this, constant<T>(other))
infix fun <T> Expr<T>.ne(other: Expr<T>): Expr<Boolean> = binary(BinaryOp.NOT_EQUAL, this, other)
infix fun <T : Any> Expr<T>.ne(other: T): Expr<Boolean> = binary(BinaryOp.NOT_EQUAL, this, constant<T>(other))

/** `whenever(condition, then, otherwise)`: only the chosen one is worked out. */
fun <T> whenever(condition: Expr<Boolean>, then: Expr<T>, otherwise: Expr<T>): Expr<T> =
    Expr(condition.ops + then.ops + otherwise.ops + Op.If)

fun <T : Any> whenever(condition: Expr<Boolean>, then: T, otherwise: T): Expr<T> =
    whenever(condition, constant<T>(then), constant<T>(otherwise))

/** The option at position `index`, counting from 0; only that one is worked out. */
fun <T> choose(index: Expr<Long>, vararg options: Expr<T>): Expr<T> =
    Expr(index.ops + options.flatMap { it.ops } + Op.Choose(options.size.toUInt()))

@JvmName("concatText")
operator fun Expr<String>.plus(other: Expr<String>): Expr<String> = binary(BinaryOp.CONCAT, this, other)
@JvmName("concatTextValue")
operator fun Expr<String>.plus(other: String): Expr<String> = binary(BinaryOp.CONCAT, this, constant<String>(other))

/** A list of these strings, in this order. */
fun texts(vararg items: Expr<String>): Expr<List<String>> =
    Expr(items.flatMap { it.ops } + Op.Texts(items.size.toUInt()))

@JvmName("concatTexts")
operator fun Expr<List<String>>.plus(other: Expr<List<String>>): Expr<List<String>> = binary(BinaryOp.CONCAT, this, other)

/** A list's items as one string. */
fun Expr<List<String>>.join(separator: String): Expr<String> = binary(BinaryOp.JOIN, this, constant<String>(separator))

// Arithmetic and ordering, for Double and for Long. They are written out
// rather than generic so that a Double never meets a Long by accident, and
// so that a whole number next to a Double expression (`price * 12`) is a
// Double.


@JvmName("plusNumber") operator fun Expr<Double>.plus(other: Expr<Double>): Expr<Double> = binary(BinaryOp.ADD, this, other)
@JvmName("plusNumberByDouble") operator fun Expr<Double>.plus(other: Double): Expr<Double> = binary(BinaryOp.ADD, this, constant<Double>(other))
@JvmName("doublePlusNumber") operator fun Double.plus(other: Expr<Double>): Expr<Double> = binary(BinaryOp.ADD, constant<Double>(this), other)
@JvmName("plusNumberByInt") operator fun Expr<Double>.plus(other: Int): Expr<Double> = binary(BinaryOp.ADD, this, constant<Double>(other.toDouble()))
@JvmName("intPlusNumber") operator fun Int.plus(other: Expr<Double>): Expr<Double> = binary(BinaryOp.ADD, constant<Double>(this.toDouble()), other)
@JvmName("minusNumber") operator fun Expr<Double>.minus(other: Expr<Double>): Expr<Double> = binary(BinaryOp.SUBTRACT, this, other)
@JvmName("minusNumberByDouble") operator fun Expr<Double>.minus(other: Double): Expr<Double> = binary(BinaryOp.SUBTRACT, this, constant<Double>(other))
@JvmName("doubleMinusNumber") operator fun Double.minus(other: Expr<Double>): Expr<Double> = binary(BinaryOp.SUBTRACT, constant<Double>(this), other)
@JvmName("minusNumberByInt") operator fun Expr<Double>.minus(other: Int): Expr<Double> = binary(BinaryOp.SUBTRACT, this, constant<Double>(other.toDouble()))
@JvmName("intMinusNumber") operator fun Int.minus(other: Expr<Double>): Expr<Double> = binary(BinaryOp.SUBTRACT, constant<Double>(this.toDouble()), other)
@JvmName("timesNumber") operator fun Expr<Double>.times(other: Expr<Double>): Expr<Double> = binary(BinaryOp.MULTIPLY, this, other)
@JvmName("timesNumberByDouble") operator fun Expr<Double>.times(other: Double): Expr<Double> = binary(BinaryOp.MULTIPLY, this, constant<Double>(other))
@JvmName("doubleTimesNumber") operator fun Double.times(other: Expr<Double>): Expr<Double> = binary(BinaryOp.MULTIPLY, constant<Double>(this), other)
@JvmName("timesNumberByInt") operator fun Expr<Double>.times(other: Int): Expr<Double> = binary(BinaryOp.MULTIPLY, this, constant<Double>(other.toDouble()))
@JvmName("intTimesNumber") operator fun Int.times(other: Expr<Double>): Expr<Double> = binary(BinaryOp.MULTIPLY, constant<Double>(this.toDouble()), other)
@JvmName("divNumber") operator fun Expr<Double>.div(other: Expr<Double>): Expr<Double> = binary(BinaryOp.DIVIDE, this, other)
@JvmName("divNumberByDouble") operator fun Expr<Double>.div(other: Double): Expr<Double> = binary(BinaryOp.DIVIDE, this, constant<Double>(other))
@JvmName("doubleDivNumber") operator fun Double.div(other: Expr<Double>): Expr<Double> = binary(BinaryOp.DIVIDE, constant<Double>(this), other)
@JvmName("divNumberByInt") operator fun Expr<Double>.div(other: Int): Expr<Double> = binary(BinaryOp.DIVIDE, this, constant<Double>(other.toDouble()))
@JvmName("intDivNumber") operator fun Int.div(other: Expr<Double>): Expr<Double> = binary(BinaryOp.DIVIDE, constant<Double>(this.toDouble()), other)
@JvmName("leastNumber") infix fun Expr<Double>.least(other: Expr<Double>): Expr<Double> = binary(BinaryOp.LEAST, this, other)
@JvmName("leastNumberByDouble") infix fun Expr<Double>.least(other: Double): Expr<Double> = binary(BinaryOp.LEAST, this, constant<Double>(other))
@JvmName("leastNumberByInt") infix fun Expr<Double>.least(other: Int): Expr<Double> = binary(BinaryOp.LEAST, this, constant<Double>(other.toDouble()))
@JvmName("greatestNumber") infix fun Expr<Double>.greatest(other: Expr<Double>): Expr<Double> = binary(BinaryOp.GREATEST, this, other)
@JvmName("greatestNumberByDouble") infix fun Expr<Double>.greatest(other: Double): Expr<Double> = binary(BinaryOp.GREATEST, this, constant<Double>(other))
@JvmName("greatestNumberByInt") infix fun Expr<Double>.greatest(other: Int): Expr<Double> = binary(BinaryOp.GREATEST, this, constant<Double>(other.toDouble()))
@JvmName("ltNumber") infix fun Expr<Double>.lt(other: Expr<Double>): Expr<Boolean> = binary(BinaryOp.LESS, this, other)
@JvmName("ltNumberByDouble") infix fun Expr<Double>.lt(other: Double): Expr<Boolean> = binary(BinaryOp.LESS, this, constant<Double>(other))
@JvmName("ltNumberByInt") infix fun Expr<Double>.lt(other: Int): Expr<Boolean> = binary(BinaryOp.LESS, this, constant<Double>(other.toDouble()))
@JvmName("leNumber") infix fun Expr<Double>.le(other: Expr<Double>): Expr<Boolean> = binary(BinaryOp.LESS_OR_EQUAL, this, other)
@JvmName("leNumberByDouble") infix fun Expr<Double>.le(other: Double): Expr<Boolean> = binary(BinaryOp.LESS_OR_EQUAL, this, constant<Double>(other))
@JvmName("leNumberByInt") infix fun Expr<Double>.le(other: Int): Expr<Boolean> = binary(BinaryOp.LESS_OR_EQUAL, this, constant<Double>(other.toDouble()))
@JvmName("gtNumber") infix fun Expr<Double>.gt(other: Expr<Double>): Expr<Boolean> = binary(BinaryOp.GREATER, this, other)
@JvmName("gtNumberByDouble") infix fun Expr<Double>.gt(other: Double): Expr<Boolean> = binary(BinaryOp.GREATER, this, constant<Double>(other))
@JvmName("gtNumberByInt") infix fun Expr<Double>.gt(other: Int): Expr<Boolean> = binary(BinaryOp.GREATER, this, constant<Double>(other.toDouble()))
@JvmName("geNumber") infix fun Expr<Double>.ge(other: Expr<Double>): Expr<Boolean> = binary(BinaryOp.GREATER_OR_EQUAL, this, other)
@JvmName("geNumberByDouble") infix fun Expr<Double>.ge(other: Double): Expr<Boolean> = binary(BinaryOp.GREATER_OR_EQUAL, this, constant<Double>(other))
@JvmName("geNumberByInt") infix fun Expr<Double>.ge(other: Int): Expr<Boolean> = binary(BinaryOp.GREATER_OR_EQUAL, this, constant<Double>(other.toDouble()))

@JvmName("plusWhole") operator fun Expr<Long>.plus(other: Expr<Long>): Expr<Long> = binary(BinaryOp.ADD, this, other)
@JvmName("plusWholeByLong") operator fun Expr<Long>.plus(other: Long): Expr<Long> = binary(BinaryOp.ADD, this, constant<Long>(other))
@JvmName("longPlusWhole") operator fun Long.plus(other: Expr<Long>): Expr<Long> = binary(BinaryOp.ADD, constant<Long>(this), other)
@JvmName("plusWholeByInt") operator fun Expr<Long>.plus(other: Int): Expr<Long> = binary(BinaryOp.ADD, this, constant<Long>(other.toLong()))
@JvmName("intPlusWhole") operator fun Int.plus(other: Expr<Long>): Expr<Long> = binary(BinaryOp.ADD, constant<Long>(this.toLong()), other)
@JvmName("minusWhole") operator fun Expr<Long>.minus(other: Expr<Long>): Expr<Long> = binary(BinaryOp.SUBTRACT, this, other)
@JvmName("minusWholeByLong") operator fun Expr<Long>.minus(other: Long): Expr<Long> = binary(BinaryOp.SUBTRACT, this, constant<Long>(other))
@JvmName("longMinusWhole") operator fun Long.minus(other: Expr<Long>): Expr<Long> = binary(BinaryOp.SUBTRACT, constant<Long>(this), other)
@JvmName("minusWholeByInt") operator fun Expr<Long>.minus(other: Int): Expr<Long> = binary(BinaryOp.SUBTRACT, this, constant<Long>(other.toLong()))
@JvmName("intMinusWhole") operator fun Int.minus(other: Expr<Long>): Expr<Long> = binary(BinaryOp.SUBTRACT, constant<Long>(this.toLong()), other)
@JvmName("timesWhole") operator fun Expr<Long>.times(other: Expr<Long>): Expr<Long> = binary(BinaryOp.MULTIPLY, this, other)
@JvmName("timesWholeByLong") operator fun Expr<Long>.times(other: Long): Expr<Long> = binary(BinaryOp.MULTIPLY, this, constant<Long>(other))
@JvmName("longTimesWhole") operator fun Long.times(other: Expr<Long>): Expr<Long> = binary(BinaryOp.MULTIPLY, constant<Long>(this), other)
@JvmName("timesWholeByInt") operator fun Expr<Long>.times(other: Int): Expr<Long> = binary(BinaryOp.MULTIPLY, this, constant<Long>(other.toLong()))
@JvmName("intTimesWhole") operator fun Int.times(other: Expr<Long>): Expr<Long> = binary(BinaryOp.MULTIPLY, constant<Long>(this.toLong()), other)
@JvmName("divWhole") operator fun Expr<Long>.div(other: Expr<Long>): Expr<Long> = binary(BinaryOp.DIVIDE, this, other)
@JvmName("divWholeByLong") operator fun Expr<Long>.div(other: Long): Expr<Long> = binary(BinaryOp.DIVIDE, this, constant<Long>(other))
@JvmName("longDivWhole") operator fun Long.div(other: Expr<Long>): Expr<Long> = binary(BinaryOp.DIVIDE, constant<Long>(this), other)
@JvmName("divWholeByInt") operator fun Expr<Long>.div(other: Int): Expr<Long> = binary(BinaryOp.DIVIDE, this, constant<Long>(other.toLong()))
@JvmName("intDivWhole") operator fun Int.div(other: Expr<Long>): Expr<Long> = binary(BinaryOp.DIVIDE, constant<Long>(this.toLong()), other)
@JvmName("leastWhole") infix fun Expr<Long>.least(other: Expr<Long>): Expr<Long> = binary(BinaryOp.LEAST, this, other)
@JvmName("leastWholeByLong") infix fun Expr<Long>.least(other: Long): Expr<Long> = binary(BinaryOp.LEAST, this, constant<Long>(other))
@JvmName("leastWholeByInt") infix fun Expr<Long>.least(other: Int): Expr<Long> = binary(BinaryOp.LEAST, this, constant<Long>(other.toLong()))
@JvmName("greatestWhole") infix fun Expr<Long>.greatest(other: Expr<Long>): Expr<Long> = binary(BinaryOp.GREATEST, this, other)
@JvmName("greatestWholeByLong") infix fun Expr<Long>.greatest(other: Long): Expr<Long> = binary(BinaryOp.GREATEST, this, constant<Long>(other))
@JvmName("greatestWholeByInt") infix fun Expr<Long>.greatest(other: Int): Expr<Long> = binary(BinaryOp.GREATEST, this, constant<Long>(other.toLong()))
@JvmName("ltWhole") infix fun Expr<Long>.lt(other: Expr<Long>): Expr<Boolean> = binary(BinaryOp.LESS, this, other)
@JvmName("ltWholeByLong") infix fun Expr<Long>.lt(other: Long): Expr<Boolean> = binary(BinaryOp.LESS, this, constant<Long>(other))
@JvmName("ltWholeByInt") infix fun Expr<Long>.lt(other: Int): Expr<Boolean> = binary(BinaryOp.LESS, this, constant<Long>(other.toLong()))
@JvmName("leWhole") infix fun Expr<Long>.le(other: Expr<Long>): Expr<Boolean> = binary(BinaryOp.LESS_OR_EQUAL, this, other)
@JvmName("leWholeByLong") infix fun Expr<Long>.le(other: Long): Expr<Boolean> = binary(BinaryOp.LESS_OR_EQUAL, this, constant<Long>(other))
@JvmName("leWholeByInt") infix fun Expr<Long>.le(other: Int): Expr<Boolean> = binary(BinaryOp.LESS_OR_EQUAL, this, constant<Long>(other.toLong()))
@JvmName("gtWhole") infix fun Expr<Long>.gt(other: Expr<Long>): Expr<Boolean> = binary(BinaryOp.GREATER, this, other)
@JvmName("gtWholeByLong") infix fun Expr<Long>.gt(other: Long): Expr<Boolean> = binary(BinaryOp.GREATER, this, constant<Long>(other))
@JvmName("gtWholeByInt") infix fun Expr<Long>.gt(other: Int): Expr<Boolean> = binary(BinaryOp.GREATER, this, constant<Long>(other.toLong()))
@JvmName("geWhole") infix fun Expr<Long>.ge(other: Expr<Long>): Expr<Boolean> = binary(BinaryOp.GREATER_OR_EQUAL, this, other)
@JvmName("geWholeByLong") infix fun Expr<Long>.ge(other: Long): Expr<Boolean> = binary(BinaryOp.GREATER_OR_EQUAL, this, constant<Long>(other))
@JvmName("geWholeByInt") infix fun Expr<Long>.ge(other: Int): Expr<Boolean> = binary(BinaryOp.GREATER_OR_EQUAL, this, constant<Long>(other.toLong()))
