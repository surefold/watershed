// watershed for Scala 3: the typed layer, which is what a program uses.
//
// An expression has a type. `qty * price` is an Expr[Double] if both are;
// `qty + region` (a number and a string) does not compile, and neither does
// setting a calculated node. Nothing is written as text.
//
//     import watershed.*
//
//     val g = Graph()
//     val qty = g.input("qty", 10.0)                     // Input[Double]
//     val price = g.input("price", 2.5)
//     val total = g.define("total", qty * price * 1.2)   // Node[Double]
//     g(price) = 3.0
//     g.stabilize()
//     val now: Double = g(total)
//
// The types a node can hold: Double, Long, BigDecimal, Boolean, String and
// List[String]. A BigDecimal is exact, holds up to 28 digits in the engine,
// and never mixes with a Double by accident.

package watershed

import dev.surefold.watershed.{Native, Wire}

import scala.annotation.{implicitNotFound, targetName}
import scala.jdk.CollectionConverters.*
import scala.math.BigDecimal.RoundingMode

/** Anything the engine refuses, with its reason. */
type WatershedException = dev.surefold.watershed.WatershedException
private def WatershedException(reason: String) = new dev.surefold.watershed.WatershedException(reason)

/** What one update did. */
final case class Report(update: Long, inputsChanged: Long, recalculated: Long, changed: Long)

// ------------------------------------------------------------------ types

/** A type a node can hold, and how its values cross into the engine. */
@implicitNotFound("a node holds a Double, Long, BigDecimal, Boolean, String or List[String], not ${T}")
sealed trait Holds[T]:
  /** The value as the JVM class the engine's Java side takes. */
  private[watershed] def out(value: T): AnyRef
  /** The value from what the engine's Java side gave. */
  private[watershed] def in(held: Any): T
  private[watershed] def set(graph: Long, node: Int, value: T): Unit
  private[watershed] def get(graph: Long, node: Int): T =
    Native.value(graph, node) match
      case failed: Native.Failed =>
        throw WatershedException(s"${Native.name(graph, node)} could not be calculated: ${failed.why}")
      case held => in(held)

object Holds:
  given Holds[Double] with
    def out(value: Double) = Double.box(value)
    def in(held: Any) = held.asInstanceOf[java.lang.Double]
    def set(graph: Long, node: Int, value: Double) = Native.setNumber(graph, node, value)
    // A Double is read as a plain number, with nothing made on the way.
    override def get(graph: Long, node: Int) = Native.number(graph, node)

  given Holds[Long] with
    def out(value: Long) = Long.box(value)
    def in(held: Any) = held.asInstanceOf[java.lang.Long]
    def set(graph: Long, node: Int, value: Long) = Native.setWhole(graph, node, value)

  given Holds[BigDecimal] with
    def out(value: BigDecimal) = value.bigDecimal
    def in(held: Any) = BigDecimal(held.asInstanceOf[java.math.BigDecimal])
    def set(graph: Long, node: Int, value: BigDecimal) = Native.setDecimal(graph, node, value.bigDecimal)

  given Holds[Boolean] with
    def out(value: Boolean) = Boolean.box(value)
    def in(held: Any) = held.asInstanceOf[java.lang.Boolean]
    def set(graph: Long, node: Int, value: Boolean) = Native.setFlag(graph, node, value)

  given Holds[String] with
    def out(value: String) = value
    def in(held: Any) = held.asInstanceOf[String]
    def set(graph: Long, node: Int, value: String) = Native.setText(graph, node, value)

  given Holds[List[String]] with
    def out(value: List[String]) = throw WatershedException("an input cannot be a list")
    def in(held: Any) = held.asInstanceOf[java.util.List[String]].asScala.toList
    def set(graph: Long, node: Int, value: List[String]) = throw WatershedException("an input cannot be a list")

/** A type with `+ - * /`, `min`, `max` and `abs`: Double, Long and BigDecimal. */
@implicitNotFound("${T} is not a number: arithmetic is for Double, Long and BigDecimal")
sealed trait Number[T]:
  private[watershed] def fromInt(whole: Int): T
object Number:
  given Number[Double] with
    def fromInt(whole: Int) = whole.toDouble
  given Number[Long] with
    def fromInt(whole: Int) = whole.toLong
  given Number[BigDecimal] with
    def fromInt(whole: Int) = BigDecimal(whole)

/** A type with `< <= > >=`: the numbers and String. */
@implicitNotFound("${T} has no order: < and > are for Double, Long, BigDecimal and String")
sealed trait Ordered[T]
object Ordered:
  given Ordered[Double] with {}
  given Ordered[Long] with {}
  given Ordered[BigDecimal] with {}
  given Ordered[String] with {}

// ------------------------------------------------------------ expressions

/** Something that works out to a `T`: a node, a constant, or a calculation over them. */
open class Expr[T] private[watershed] (private[watershed] val steps: Array[Byte])

/** A node of a graph holding a `T`. */
open class Node[T] private[watershed] (private[watershed] val graph: Graph, val id: Int)
    extends Expr[T](Wire.node(id)):
  override def equals(other: Any) = other match
    case node: Node[?] => (node.graph eq graph) && node.id == id
    case _             => false
  override def hashCode = id.hashCode
  override def toString = s"Node($id)"

/** An input: a node that can be set. */
final class Input[T] private[watershed] (graph: Graph, id: Int) extends Node[T](graph, id)

/** A field or calculated column of a collection's rows, holding a `T` in each row. */
open class Part[T] private[watershed] (val name: String) extends Expr[T](Wire.part(name))

/** A field: the part of a row that is given, not calculated. */
final class Field[T] private[watershed] (name: String)(using holds: Holds[T]) extends Part[T](name):
  /** This field's value for a row: `orders.upsert("o-1", qty := 2.0)`. */
  def :=(value: T): FieldValue = FieldValue(name, holds.out(value))

/** A field's value for a row, made with `:=`. */
final class FieldValue private[watershed] (private[watershed] val name: String, private[watershed] val value: AnyRef)

/** A row to add or change, for `Collection.upsertAll`: `row("o-1", qty := 2.0, price := 9.99)`. */
final class Row private[watershed] (private[watershed] val key: String, private[watershed] val fields: Seq[FieldValue])

def row(key: String, fields: FieldValue*): Row = Row(key, fields)

private def constant[T](value: T)(using holds: Holds[T]): Expr[T] =
  Expr(Wire.constant(holds.out(value)))

private def unary[T](op: Byte, a: Expr[?]): Expr[T] =
  Expr(Wire.unary(op, a.steps))

private def binary[T](op: Byte, a: Expr[?], b: Expr[?]): Expr[T] =
  Expr(Wire.binary(op, a.steps, b.steps))

/** A constant, for where a value alone would not say which type is meant. */
def value[T: Holds](value: T): Expr[T] = constant(value)

extension [T](a: Expr[T])(using Number[T], Holds[T])
  def +(b: Expr[T]): Expr[T] = binary(Wire.ADD, a, b)
  def +(b: T): Expr[T] = binary(Wire.ADD, a, constant(b))
  def -(b: Expr[T]): Expr[T] = binary(Wire.SUBTRACT, a, b)
  def -(b: T): Expr[T] = binary(Wire.SUBTRACT, a, constant(b))
  def *(b: Expr[T]): Expr[T] = binary(Wire.MULTIPLY, a, b)
  def *(b: T): Expr[T] = binary(Wire.MULTIPLY, a, constant(b))
  def /(b: Expr[T]): Expr[T] = binary(Wire.DIVIDE, a, b)
  def /(b: T): Expr[T] = binary(Wire.DIVIDE, a, constant(b))
  def min(b: Expr[T]): Expr[T] = binary(Wire.LEAST, a, b)
  def min(b: T): Expr[T] = binary(Wire.LEAST, a, constant(b))
  def max(b: Expr[T]): Expr[T] = binary(Wire.GREATEST, a, b)
  def max(b: T): Expr[T] = binary(Wire.GREATEST, a, constant(b))
  def unary_- : Expr[T] = unary(Wire.NEGATE, a)
  def abs: Expr[T] = unary(Wire.ABSOLUTE, a)

extension [T](a: Expr[T])(using Ordered[T], Holds[T])
  def <(b: Expr[T]): Expr[Boolean] = binary(Wire.LESS, a, b)
  def <(b: T): Expr[Boolean] = binary(Wire.LESS, a, constant(b))
  def <=(b: Expr[T]): Expr[Boolean] = binary(Wire.LESS_OR_EQUAL, a, b)
  def <=(b: T): Expr[Boolean] = binary(Wire.LESS_OR_EQUAL, a, constant(b))
  def >(b: Expr[T]): Expr[Boolean] = binary(Wire.GREATER, a, b)
  def >(b: T): Expr[Boolean] = binary(Wire.GREATER, a, constant(b))
  def >=(b: Expr[T]): Expr[Boolean] = binary(Wire.GREATER_OR_EQUAL, a, b)
  def >=(b: T): Expr[Boolean] = binary(Wire.GREATER_OR_EQUAL, a, constant(b))

// `==` must give a Boolean in Scala, so equality that builds an expression is `===`.
extension [T](a: Expr[T])(using Holds[T])
  def ===(b: Expr[T]): Expr[Boolean] = binary(Wire.EQUAL, a, b)
  def ===(b: T): Expr[Boolean] = binary(Wire.EQUAL, a, constant(b))
  def =!=(b: Expr[T]): Expr[Boolean] = binary(Wire.NOT_EQUAL, a, b)
  def =!=(b: T): Expr[Boolean] = binary(Wire.NOT_EQUAL, a, constant(b))

extension (a: Expr[Boolean])
  def &&(b: Expr[Boolean]): Expr[Boolean] = binary(Wire.AND, a, b)
  def ||(b: Expr[Boolean]): Expr[Boolean] = binary(Wire.OR, a, b)
  def unary_! : Expr[Boolean] = unary(Wire.NOT, a)

extension (a: Expr[String])
  /** This text followed by that. */
  def ++(b: Expr[String]): Expr[String] = binary(Wire.CONCAT, a, b)
  def ++(b: String): Expr[String] = binary(Wire.CONCAT, a, constant(b))

extension (a: Expr[List[String]])
  /** This list followed by that. */
  @targetName("concatTexts")
  def ++(b: Expr[List[String]]): Expr[List[String]] = binary(Wire.CONCAT, a, b)

  /** The items as one string. */
  def join(separator: String): Expr[String] = binary(Wire.JOIN, a, constant(separator))

extension (a: Expr[BigDecimal])
  /** To this many decimal places (0 to 28). A tie goes to the even neighbour unless another way is given. */
  def round(places: Int, rounding: RoundingMode.Value = RoundingMode.HALF_EVEN): Expr[BigDecimal] =
    val op = rounding match
      case RoundingMode.HALF_EVEN => Wire.ROUND_HALF_EVEN
      case RoundingMode.HALF_UP   => Wire.ROUND_HALF_UP
      case RoundingMode.DOWN      => Wire.ROUND_DOWN
      case RoundingMode.UP        => Wire.ROUND_UP
      case other => throw IllegalArgumentException(s"rounding is HALF_EVEN, HALF_UP, DOWN or UP, not $other")
    binary(op, a, constant(places.toLong))

  /** The nearest Double, for where exactness is not needed. */
  def toDouble: Expr[Double] = unary(Wire.TO_NUMBER, a)

extension (a: Expr[Long])
  /** The same number as a decimal, exactly. */
  def toDecimal: Expr[BigDecimal] = unary(Wire.TO_DECIMAL, a)

// A constant on the left, as in `1 + taxRate`.
extension (x: Double)
  @targetName("doublePlus") def +(b: Expr[Double]): Expr[Double] = constant(x) + b
  @targetName("doubleMinus") def -(b: Expr[Double]): Expr[Double] = constant(x) - b
  @targetName("doubleTimes") def *(b: Expr[Double]): Expr[Double] = constant(x) * b
  @targetName("doubleOver") def /(b: Expr[Double]): Expr[Double] = constant(x) / b

extension (x: Long)
  @targetName("longPlus") def +(b: Expr[Long]): Expr[Long] = constant(x) + b
  @targetName("longMinus") def -(b: Expr[Long]): Expr[Long] = constant(x) - b
  @targetName("longTimes") def *(b: Expr[Long]): Expr[Long] = constant(x) * b
  @targetName("longOver") def /(b: Expr[Long]): Expr[Long] = constant(x) / b

extension (x: BigDecimal)
  @targetName("decimalPlus") def +(b: Expr[BigDecimal]): Expr[BigDecimal] = constant(x) + b
  @targetName("decimalMinus") def -(b: Expr[BigDecimal]): Expr[BigDecimal] = constant(x) - b
  @targetName("decimalTimes") def *(b: Expr[BigDecimal]): Expr[BigDecimal] = constant(x) * b
  @targetName("decimalOver") def /(b: Expr[BigDecimal]): Expr[BigDecimal] = constant(x) / b

// A whole number written on the left is taken as whichever number the
// expression is: `1 + taxRate` with a Double, a Long or a BigDecimal.
extension (x: Int)
  @targetName("intPlus") def +[T](b: Expr[T])(using number: Number[T], holds: Holds[T]): Expr[T] =
    constant(number.fromInt(x)) + b
  @targetName("intMinus") def -[T](b: Expr[T])(using number: Number[T], holds: Holds[T]): Expr[T] =
    constant(number.fromInt(x)) - b
  @targetName("intTimes") def *[T](b: Expr[T])(using number: Number[T], holds: Holds[T]): Expr[T] =
    constant(number.fromInt(x)) * b
  @targetName("intOver") def /[T](b: Expr[T])(using number: Number[T], holds: Holds[T]): Expr[T] =
    constant(number.fromInt(x)) / b

/** `whenever(condition, then, otherwise)`: only the chosen one is worked out. */
def whenever[T](condition: Expr[Boolean], thenValue: Expr[T], otherwise: Expr[T]): Expr[T] =
  Expr(Wire.choice(condition.steps, thenValue.steps, otherwise.steps))

@targetName("wheneverValues")
def whenever[T: Holds](condition: Expr[Boolean], thenValue: T, otherwise: T): Expr[T] =
  whenever(condition, constant(thenValue), constant(otherwise))

/** The option at position `index`, counting from 0; only that one is worked out. */
def choose[T](index: Expr[Long], options: Expr[T]*): Expr[T] =
  Expr(Wire.choose(index.steps, options.map(_.steps).asJava))

/** Any value as text. */
def text(a: Expr[?]): Expr[String] = unary(Wire.TO_TEXT, a)

/** A list of these strings, in this order. */
def texts(items: Expr[String]*): Expr[List[String]] =
  Expr(Wire.texts(items.map(_.steps).asJava))

// ------------------------------------------------------------------ graph

/** A graph of inputs and calculated nodes. */
final class Graph:
  // Where the graph is in the engine. It is freed when this object can no longer be reached.
  private val address: Long = Native.graph(this)

  private def mine[N <: Node[?]](node: N): N =
    if node.graph ne this then throw WatershedException("that node belongs to another graph")
    node

  /** Adds an input with a starting value. */
  def input[T](name: String, value: T)(using holds: Holds[T]): Input[T] =
    Input(this, Native.input(address, name, holds.out(value)))

  /** Adds a calculated node. */
  def define[T](name: String, expression: Expr[T]): Node[T] =
    Node(this, Native.define(address, name, expression.steps))

  /** Adds many calculated nodes in one call, in order. */
  def defineAll[T](definitions: Seq[(String, Expr[T])]): Seq[Node[T]] =
    Native
      .defineMany(address, definitions.map(_._1).asJava, definitions.map(_._2.steps).asJava)
      .toSeq
      .map(id => Node[T](this, id))

  /** Records a new value for an input, `g(price) = 3.0`; it counts from the next `stabilize()`. */
  def update[T](input: Input[T], value: T)(using holds: Holds[T]): Unit =
    holds.set(address, mine(input).id, value)

  /** Sets many Double inputs in one call: the way to apply a batch of prices. */
  def setAll(values: Iterable[(Input[Double], Double)]): Unit =
    Native.setNumbers(address, values.iterator.map(v => mine(v._1).id).toArray, values.iterator.map(_._2).toArray)

  /** The node's value as of the last update, `g(total)`. A node that could not be calculated throws, with why. */
  def apply[T](node: Node[T])(using holds: Holds[T]): T = holds.get(address, mine(node).id)

  /** The node's value, or None if it could not be calculated. */
  def get[T](node: Node[T])(using holds: Holds[T]): Option[T] =
    Native.value(address, mine(node).id) match
      case _: Native.Failed => None
      case held             => Some(holds.in(held))

  /** Applies every recorded change and recalculates what depends on them, and nothing else. */
  def stabilize(): Report =
    val report = Native.stabilize(address)
    Report(report.update, report.inputsChanged, report.recalculated, report.changed)

  /** Why the node has its value, as a tree of what it was worked out from. */
  def explain(node: Node[?]): String = Native.explain(address, mine(node).id)

  /** The names of the nodes the last update recalculated, in order. */
  def lastRecalculated: List[String] = Native.lastRecalculated(address).asScala.toList

  /** How many nodes the graph has had. */
  def size: Long = Native.size(address)

  /** Removes a node that nothing uses; its name is then free. */
  def remove(node: Node[?]): Unit = Native.remove(address, mine(node).id)

  /** Watches a node: in pull mode, it and what it uses are kept up to date. */
  def watch(node: Node[?]): Unit = Native.watch(address, mine(node).id, true)
  def unwatch(node: Node[?]): Unit = Native.watch(address, mine(node).id, false)

  /** Pull mode: while on, an update calculates only the watched nodes and what they use. */
  def calculateOnlyWatched(on: Boolean): Unit = Native.calculateOnlyWatched(address, on)

  /** Checks every node against a calculation from scratch. */
  def checkConsistency(): Unit = Native.checkConsistency(address)

  /** A new, empty keyed table of rows in this graph. */
  def collection(name: String): Collection = Collection(this, address, name)

// -------------------------------------------------------------- collection

/** A keyed table of rows, with fields, calculated columns and totals that update by only the rows that change. */
final class Collection private[watershed] (graph: Graph, graphAddress: Long, name: String):
  // Holding `graph` keeps the graph alive for as long as this is.
  private val address: Long = Native.collection(this, graphAddress, name)

  private def field[T: Holds](name: String, kind: Byte): Field[T] =
    Native.field(address, name, kind)
    Field(name)

  def number(name: String): Field[Double] = field(name, Wire.NUMBER)
  def whole(name: String): Field[Long] = field(name, Wire.WHOLE)
  def decimal(name: String): Field[BigDecimal] = field(name, Wire.DECIMAL)
  def flag(name: String): Field[Boolean] = field(name, Wire.FLAG)
  def text(name: String): Field[String] = field(name, Wire.TEXT)

  /** Adds a column, worked out in every row from its fields, other columns and any node. */
  def column[T](name: String, expression: Expr[T]): Part[T] =
    Native.column(address, name, expression.steps)
    Part(name)

  def sum[T: Number](name: String, of: Part[T]): Node[T] = Node(graph, Native.total(address, name, Wire.SUM, of.name))
  def min[T: Number](name: String, of: Part[T]): Node[T] = Node(graph, Native.total(address, name, Wire.SMALLEST, of.name))
  def max[T: Number](name: String, of: Part[T]): Node[T] = Node(graph, Native.total(address, name, Wire.LARGEST, of.name))
  def count(name: String): Node[Long] = Node(graph, Native.total(address, name, Wire.COUNT, null))

  private def write(to: Wire, key: String, fields: Seq[FieldValue]): Unit =
    to.put(key).put(fields.size)
    fields.foreach(field => to.put(field.name).value(field.value))

  /** Adds a row, or changes the fields given of one that is there: `orders.upsert("o-1", qty := 2.0)`. */
  def upsert(key: String, fields: FieldValue*): Unit =
    val wire = Wire()
    write(wire, key, fields)
    Native.upsert(address, wire.bytes(), 1)

  /** Adds or changes many rows in one call: `orders.upsertAll(Seq(row("o-1", qty := 2.0), ...))`. */
  def upsertAll(rows: Iterable[Row]): Unit =
    val wire = Wire()
    rows.foreach(row => write(wire, row.key, row.fields))
    Native.upsert(address, wire.bytes(), rows.size)

  /** Removes a row. Says whether there was one. */
  def remove(key: String): Boolean = Native.removeRow(address, key)

  /** A field or column of a row, as of the last update: `orders("o-1", line)`. */
  def apply[T](key: String, part: Part[T])(using holds: Holds[T]): T =
    Native.rowValue(address, key, part.name) match
      case failed: Native.Failed =>
        throw WatershedException(s"$key.${part.name} could not be calculated: ${failed.why}")
      case held => holds.in(held)

  /** How many rows there are, counting changes since the last update. */
  def size: Long = Native.rows(address)
