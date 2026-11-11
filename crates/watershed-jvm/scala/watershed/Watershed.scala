// watershed for Scala 3: a typed layer over the JVM bindings.
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

import dev.surefold.watershed as raw

import scala.annotation.{implicitNotFound, targetName}
import scala.jdk.CollectionConverters.*
import scala.math.BigDecimal.RoundingMode

/** Anything the engine refuses, with its reason. */
type WatershedException = raw.WatershedException

/** What one update did. */
final case class Report(update: Long, inputsChanged: Long, recalculated: Long, changed: Long)

// ------------------------------------------------------------------ types

/** A type a node can hold, and how its values cross into the engine. */
@implicitNotFound("a node holds a Double, Long, BigDecimal, Boolean, String or List[String], not ${T}")
sealed trait Holds[T]:
  private[watershed] def toValue(value: T): raw.Value
  private[watershed] def fromValue(value: raw.Value): T
  private[watershed] def set(graph: raw.Graph, node: Int, value: T): Unit =
    graph.setValue(node, toValue(value))
  private[watershed] def get(graph: raw.Graph, node: Int): T =
    graph.value(node) match
      case failed: raw.Value.Failed =>
        throw raw.WatershedException.Refused(s"${graph.name(node)} could not be calculated: ${failed.getMessage}")
      case held => fromValue(held)

object Holds:
  private def unexpected(value: raw.Value): Nothing =
    throw raw.WatershedException.Refused(s"the engine gave $value where another type was expected")

  given Holds[Double] with
    def toValue(value: Double) = raw.Value.Number(value)
    def fromValue(value: raw.Value) = value match
      case n: raw.Value.Number => n.getValue
      case other               => unexpected(other)
    // A Double crosses as a plain number, far faster than any other value.
    override def set(graph: raw.Graph, node: Int, value: Double) = graph.setNumber(node, value)
    override def get(graph: raw.Graph, node: Int) = graph.number(node)

  given Holds[Long] with
    def toValue(value: Long) = raw.Value.Whole(value)
    def fromValue(value: raw.Value) = value match
      case n: raw.Value.Whole => n.getValue
      case other              => unexpected(other)

  given Holds[BigDecimal] with
    // Through its digits, written out in full, so nothing is lost on the way.
    def toValue(value: BigDecimal) = raw.Value.Decimal(value.bigDecimal.toPlainString)
    def fromValue(value: raw.Value) = value match
      case n: raw.Value.Decimal => BigDecimal(java.math.BigDecimal(n.getValue))
      case other                => unexpected(other)

  given Holds[Boolean] with
    def toValue(value: Boolean) = raw.Value.Flag(value)
    def fromValue(value: raw.Value) = value match
      case n: raw.Value.Flag => n.getValue
      case other             => unexpected(other)

  given Holds[String] with
    def toValue(value: String) = raw.Value.Text(value)
    def fromValue(value: raw.Value) = value match
      case n: raw.Value.Text => n.getValue
      case other             => unexpected(other)

  given Holds[List[String]] with
    def toValue(value: List[String]) = raw.Value.Texts(value.asJava)
    def fromValue(value: raw.Value) = value match
      case n: raw.Value.Texts => n.getItems.asScala.toList
      case other              => unexpected(other)

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
open class Expr[T] private[watershed] (private[watershed] val ops: List[raw.Op])

/** A node of a graph holding a `T`. */
open class Node[T] private[watershed] (val id: Int) extends Expr[T](List(raw.Op.Node(id))):
  override def equals(other: Any) = other match
    case node: Node[?] => node.id == id
    case _             => false
  override def hashCode = id.hashCode
  override def toString = s"Node($id)"

/** An input: a node that can be set. */
final class Input[T] private[watershed] (id: Int) extends Node[T](id)

/** A field or calculated column of a collection's rows, holding a `T` in each row. */
open class Part[T] private[watershed] (val name: String) extends Expr[T](List(raw.Op.Part(name)))

/** A field: the part of a row that is given, not calculated. */
final class Field[T] private[watershed] (name: String)(using holds: Holds[T]) extends Part[T](name):
  /** This field's value for a row: `orders.upsert("o-1", qty := 2.0)`. */
  def :=(value: T): FieldValue = FieldValue(name, holds.toValue(value))

/** A field's value for a row, made with `:=`. */
final class FieldValue private[watershed] (private[watershed] val name: String, private[watershed] val value: raw.Value)

private def constant[T](value: T)(using holds: Holds[T]): Expr[T] =
  Expr(List(raw.Op.Constant(holds.toValue(value))))

private def unary[T](op: raw.UnaryOp, a: Expr[?]): Expr[T] =
  Expr(a.ops :+ raw.Op.Unary(op))

private def binary[T](op: raw.BinaryOp, a: Expr[?], b: Expr[?]): Expr[T] =
  Expr(a.ops ++ b.ops :+ raw.Op.Binary(op))

/** A constant, for where a value alone would not say which type is meant. */
def value[T: Holds](value: T): Expr[T] = constant(value)

extension [T](a: Expr[T])(using Number[T], Holds[T])
  def +(b: Expr[T]): Expr[T] = binary(raw.BinaryOp.ADD, a, b)
  def +(b: T): Expr[T] = binary(raw.BinaryOp.ADD, a, constant(b))
  def -(b: Expr[T]): Expr[T] = binary(raw.BinaryOp.SUBTRACT, a, b)
  def -(b: T): Expr[T] = binary(raw.BinaryOp.SUBTRACT, a, constant(b))
  def *(b: Expr[T]): Expr[T] = binary(raw.BinaryOp.MULTIPLY, a, b)
  def *(b: T): Expr[T] = binary(raw.BinaryOp.MULTIPLY, a, constant(b))
  def /(b: Expr[T]): Expr[T] = binary(raw.BinaryOp.DIVIDE, a, b)
  def /(b: T): Expr[T] = binary(raw.BinaryOp.DIVIDE, a, constant(b))
  def min(b: Expr[T]): Expr[T] = binary(raw.BinaryOp.LEAST, a, b)
  def min(b: T): Expr[T] = binary(raw.BinaryOp.LEAST, a, constant(b))
  def max(b: Expr[T]): Expr[T] = binary(raw.BinaryOp.GREATEST, a, b)
  def max(b: T): Expr[T] = binary(raw.BinaryOp.GREATEST, a, constant(b))
  def unary_- : Expr[T] = unary(raw.UnaryOp.NEGATE, a)
  def abs: Expr[T] = unary(raw.UnaryOp.ABSOLUTE, a)

extension [T](a: Expr[T])(using Ordered[T], Holds[T])
  def <(b: Expr[T]): Expr[Boolean] = binary(raw.BinaryOp.LESS, a, b)
  def <(b: T): Expr[Boolean] = binary(raw.BinaryOp.LESS, a, constant(b))
  def <=(b: Expr[T]): Expr[Boolean] = binary(raw.BinaryOp.LESS_OR_EQUAL, a, b)
  def <=(b: T): Expr[Boolean] = binary(raw.BinaryOp.LESS_OR_EQUAL, a, constant(b))
  def >(b: Expr[T]): Expr[Boolean] = binary(raw.BinaryOp.GREATER, a, b)
  def >(b: T): Expr[Boolean] = binary(raw.BinaryOp.GREATER, a, constant(b))
  def >=(b: Expr[T]): Expr[Boolean] = binary(raw.BinaryOp.GREATER_OR_EQUAL, a, b)
  def >=(b: T): Expr[Boolean] = binary(raw.BinaryOp.GREATER_OR_EQUAL, a, constant(b))

// `==` must give a Boolean in Scala, so equality that builds an expression is `===`.
extension [T](a: Expr[T])(using Holds[T])
  def ===(b: Expr[T]): Expr[Boolean] = binary(raw.BinaryOp.EQUAL, a, b)
  def ===(b: T): Expr[Boolean] = binary(raw.BinaryOp.EQUAL, a, constant(b))
  def =!=(b: Expr[T]): Expr[Boolean] = binary(raw.BinaryOp.NOT_EQUAL, a, b)
  def =!=(b: T): Expr[Boolean] = binary(raw.BinaryOp.NOT_EQUAL, a, constant(b))

extension (a: Expr[Boolean])
  def &&(b: Expr[Boolean]): Expr[Boolean] = binary(raw.BinaryOp.AND, a, b)
  def ||(b: Expr[Boolean]): Expr[Boolean] = binary(raw.BinaryOp.OR, a, b)
  def unary_! : Expr[Boolean] = unary(raw.UnaryOp.NOT, a)

extension (a: Expr[String])
  /** This text followed by that. */
  def ++(b: Expr[String]): Expr[String] = binary(raw.BinaryOp.CONCAT, a, b)
  def ++(b: String): Expr[String] = binary(raw.BinaryOp.CONCAT, a, constant(b))

extension (a: Expr[List[String]])
  /** This list followed by that. */
  @targetName("concatTexts")
  def ++(b: Expr[List[String]]): Expr[List[String]] = binary(raw.BinaryOp.CONCAT, a, b)

  /** The items as one string. */
  def join(separator: String): Expr[String] = binary(raw.BinaryOp.JOIN, a, constant(separator))

extension (a: Expr[BigDecimal])
  /** To this many decimal places (0 to 28). A tie goes to the even neighbour unless another way is given. */
  def round(places: Int, rounding: RoundingMode.Value = RoundingMode.HALF_EVEN): Expr[BigDecimal] =
    val op = rounding match
      case RoundingMode.HALF_EVEN => raw.BinaryOp.ROUND_HALF_EVEN
      case RoundingMode.HALF_UP   => raw.BinaryOp.ROUND_HALF_UP
      case RoundingMode.DOWN      => raw.BinaryOp.ROUND_DOWN
      case RoundingMode.UP        => raw.BinaryOp.ROUND_UP
      case other => throw IllegalArgumentException(s"rounding is HALF_EVEN, HALF_UP, DOWN or UP, not $other")
    binary(op, a, constant(places.toLong))

  /** The nearest Double, for where exactness is not needed. */
  def toDouble: Expr[Double] = unary(raw.UnaryOp.TO_NUMBER, a)

extension (a: Expr[Long])
  /** The same number as a decimal, exactly. */
  def toDecimal: Expr[BigDecimal] = unary(raw.UnaryOp.TO_DECIMAL, a)

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
  Expr(condition.ops ++ thenValue.ops ++ otherwise.ops :+ raw.Op.If.INSTANCE)

@targetName("wheneverValues")
def whenever[T: Holds](condition: Expr[Boolean], thenValue: T, otherwise: T): Expr[T] =
  whenever(condition, constant(thenValue), constant(otherwise))

/** The option at position `index`, counting from 0; only that one is worked out. */
def choose[T](index: Expr[Long], options: Expr[T]*): Expr[T] =
  Expr(index.ops ++ options.flatMap(_.ops) :+ raw.Op.Choose(options.size))

/** Any value as text. */
def text(a: Expr[?]): Expr[String] = unary(raw.UnaryOp.TO_TEXT, a)

/** A list of these strings, in this order. */
def texts(items: Expr[String]*): Expr[List[String]] =
  Expr(items.toList.flatMap(_.ops) :+ raw.Op.Texts(items.size))

// ------------------------------------------------------------------ graph

/** A graph of inputs and calculated nodes. */
final class Graph:
  private val graph = raw.Graph()

  /** Adds an input with a starting value. */
  def input[T](name: String, value: T)(using holds: Holds[T]): Input[T] =
    Input(graph.inputValue(name, holds.toValue(value)))

  /** Adds a calculated node. */
  def define[T](name: String, expression: Expr[T]): Node[T] =
    Node(graph.defineExpression(name, expression.ops.asJava))

  /** Adds many calculated nodes in one call, in order. Far faster than one call each when there are thousands. */
  def defineAll[T](definitions: Seq[(String, Expr[T])]): Seq[Node[T]] =
    graph
      .defineExpressions(definitions.map(_._1).asJava, definitions.map(_._2.ops.asJava).asJava)
      .asScala
      .map(id => Node[T](id))
      .toSeq

  /** Records a new value for an input, `g(price) = 3.0`; it counts from the next `stabilize()`. */
  def update[T](input: Input[T], value: T)(using holds: Holds[T]): Unit =
    holds.set(graph, input.id, value)

  /** Sets many Double inputs in one call: the way to apply a batch of prices. */
  def setAll(values: Iterable[(Input[Double], Double)]): Unit =
    graph.setNumbers(
      values.map(v => Int.box(v._1.id)).toSeq.asJava,
      values.map(v => Double.box(v._2)).toSeq.asJava,
    )

  /** The node's value as of the last update, `g(total)`. A node that could not be calculated throws, with why. */
  def apply[T](node: Node[T])(using holds: Holds[T]): T = holds.get(graph, node.id)

  /** The node's value, or None if it could not be calculated. */
  def get[T](node: Node[T])(using holds: Holds[T]): Option[T] =
    graph.value(node.id) match
      case _: raw.Value.Failed => None
      case held                => Some(holds.fromValue(held))

  /** Applies every recorded change and recalculates what depends on them, and nothing else. */
  def stabilize(): Report =
    val report = graph.stabilize()
    Report(report.getUpdate, report.getInputsChanged, report.getRecalculated, report.getChanged)

  /** Why the node has its value, as a tree of what it was worked out from. */
  def explain(node: Node[?]): String = graph.explain(node.id)

  /** The names of the nodes the last update recalculated, in order. */
  def lastRecalculated: List[String] = graph.lastRecalculated().asScala.toList

  /** How many nodes the graph has had. */
  def size: Long = graph.size()

  /** Removes a node that nothing uses; its name is then free. */
  def remove(node: Node[?]): Unit = graph.remove(node.id)

  /** Watches a node: in pull mode, it and what it uses are kept up to date. */
  def watch(node: Node[?]): Unit = graph.watch(node.id)
  def unwatch(node: Node[?]): Unit = graph.unwatch(node.id)

  /** Pull mode: while on, an update calculates only the watched nodes and what they use. */
  def calculateOnlyWatched(on: Boolean): Unit = graph.calculateOnlyWatched(on)

  /** Checks every node against a calculation from scratch. */
  def checkConsistency(): Unit = graph.checkConsistency()

  /** A new, empty keyed table of rows in this graph. */
  def collection(name: String): Collection = Collection(graph.collection(name))

// -------------------------------------------------------------- collection

/** A keyed table of rows, with fields, calculated columns and totals that update by only the rows that change. */
final class Collection private[watershed] (collection: raw.Collection):
  private def field[T: Holds](name: String, kind: raw.Kind): Field[T] =
    collection.addField(name, kind)
    Field(name)

  def number(name: String): Field[Double] = field(name, raw.Kind.NUMBER)
  def whole(name: String): Field[Long] = field(name, raw.Kind.WHOLE)
  def decimal(name: String): Field[BigDecimal] = field(name, raw.Kind.DECIMAL)
  def flag(name: String): Field[Boolean] = field(name, raw.Kind.FLAG)
  def text(name: String): Field[String] = field(name, raw.Kind.TEXT)

  /** Adds a column, worked out in every row from its fields, other columns and any node. */
  def column[T](name: String, expression: Expr[T]): Part[T] =
    collection.addColumn(name, expression.ops.asJava)
    Part(name)

  def sum[T: Number](name: String, of: Part[T]): Node[T] = Node(collection.addSum(name, of.name))
  def min[T: Number](name: String, of: Part[T]): Node[T] = Node(collection.addMin(name, of.name))
  def max[T: Number](name: String, of: Part[T]): Node[T] = Node(collection.addMax(name, of.name))
  def count(name: String): Node[Long] = Node(collection.addCount(name))

  /** Adds a row, or changes the fields given of one that is there: `orders.upsert("o-1", qty := 2.0)`. */
  def upsert(key: String, fields: FieldValue*): Unit =
    collection.upsertValues(key, fields.map(f => f.name -> f.value).toMap.asJava)

  /** Removes a row. Says whether there was one. */
  def remove(key: String): Boolean = collection.remove(key)

  /** A field or column of a row, as of the last update: `orders("o-1", line)`. */
  def apply[T](key: String, part: Part[T])(using holds: Holds[T]): T =
    collection.value(key, part.name) match
      case failed: raw.Value.Failed =>
        throw raw.WatershedException.Refused(s"$key.${part.name} could not be calculated: ${failed.getMessage}")
      case held => holds.fromValue(held)

  /** How many rows there are, counting changes since the last update. */
  def size: Long = collection.size()
