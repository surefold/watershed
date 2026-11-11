// The calls a running program makes over and over, made directly.
//
// The generated bindings go through JNA, about 10 µs a call. These go
// through the JDK's foreign function API (java.lang.foreign, JDK 22 on) to
// the plain C functions in the Rust library's fast.rs, in tens of
// nanoseconds. They only say whether a call worked; when one did not, the
// typed layers make the same call through the generated bindings, which
// raises the error with its reason.
//
// Used by the typed Kotlin and Scala layers. Not for calling directly.

package dev.surefold.watershed

import java.lang.foreign.Arena
import java.lang.foreign.FunctionDescriptor
import java.lang.foreign.Linker
import java.lang.foreign.MemorySegment
import java.lang.foreign.SymbolLookup
import java.lang.foreign.ValueLayout.ADDRESS
import java.lang.foreign.ValueLayout.JAVA_DOUBLE
import java.lang.foreign.ValueLayout.JAVA_INT
import java.lang.foreign.ValueLayout.JAVA_LONG
import java.lang.invoke.MethodHandle

object Fast {
    private const val DONE = 0

    // The same file JNA loaded for the generated bindings.
    private val library: SymbolLookup = SymbolLookup.libraryLookup(
        com.sun.jna.NativeLibrary.getInstance("watershed_jvm").file.toPath(),
        Arena.global(),
    )

    private fun function(name: String, shape: FunctionDescriptor): MethodHandle =
        Linker.nativeLinker().downcallHandle(library.find(name).orElseThrow(), shape)

    private val setNumber = function("watershed_set_number", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT, JAVA_DOUBLE))
    private val setNumbers = function("watershed_set_numbers", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS, ADDRESS, JAVA_LONG))
    private val number = function("watershed_number", FunctionDescriptor.of(JAVA_DOUBLE, JAVA_LONG, JAVA_INT))
    private val stabilize = function("watershed_stabilize", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS))

    // Room for one update's report, per thread, so an update allocates nothing.
    private val reports: ThreadLocal<MemorySegment> =
        ThreadLocal.withInitial { Arena.global().allocate(JAVA_LONG, 4) }

    /** Records a new value for a number input. False: make the call the slow way. */
    @JvmStatic
    fun setNumber(graph: Long, node: Int, value: Double): Boolean =
        setNumber.invokeExact(graph, node, value) as Int == DONE

    /** Records new values for many number inputs. False: make the call the slow way. */
    @JvmStatic
    fun setNumbers(graph: Long, nodes: IntArray, values: DoubleArray): Boolean {
        require(nodes.size == values.size) { "${nodes.size} nodes were given ${values.size} values" }
        Arena.ofConfined().use { arena ->
            val nodesThere = arena.allocateFrom(JAVA_INT, *nodes)
            val valuesThere = arena.allocateFrom(JAVA_DOUBLE, *values)
            return setNumbers.invokeExact(graph, nodesThere, valuesThere, nodes.size.toLong()) as Int == DONE
        }
    }

    /** A number node's value, or NaN to say: read it the slow way. */
    @JvmStatic
    fun number(graph: Long, node: Int): Double = number.invokeExact(graph, node) as Double

    /** Runs an update. Null: make the call the slow way. */
    @JvmStatic
    fun stabilize(graph: Long): Report? {
        val report = reports.get()
        if (stabilize.invokeExact(graph, report) as Int != DONE) return null
        return Report(
            report.getAtIndex(JAVA_LONG, 0),
            report.getAtIndex(JAVA_LONG, 1),
            report.getAtIndex(JAVA_LONG, 2),
            report.getAtIndex(JAVA_LONG, 3),
        )
    }
}
