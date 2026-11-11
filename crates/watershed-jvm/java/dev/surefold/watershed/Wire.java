package dev.surefold.watershed;

import java.math.BigDecimal;
import java.math.BigInteger;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.List;

/**
 * Writes values, expressions and rows as the bytes the engine reads (see
 * {@code src/wire.rs}): whole numbers little-endian, text as its length and
 * then UTF-8.
 *
 * <p>Used by the typed Kotlin and Scala layers. Not for calling directly.
 */
public final class Wire {
    // The kinds of value, as the engine numbers them.
    public static final byte NUMBER = 0, WHOLE = 1, DECIMAL = 2, FLAG = 3, TEXT = 4, TEXTS = 5, FAILED = 6;

    // Operations on one value.
    public static final byte NEGATE = 0, NOT = 1, ABSOLUTE = 2, TO_TEXT = 3, TO_DECIMAL = 4, TO_NUMBER = 5;

    // Operations on two values.
    public static final byte ADD = 0, SUBTRACT = 1, MULTIPLY = 2, DIVIDE = 3, LEAST = 4, GREATEST = 5,
            LESS = 6, LESS_OR_EQUAL = 7, GREATER = 8, GREATER_OR_EQUAL = 9, EQUAL = 10, NOT_EQUAL = 11,
            AND = 12, OR = 13, CONCAT = 14, JOIN = 15,
            ROUND_HALF_EVEN = 16, ROUND_HALF_UP = 17, ROUND_DOWN = 18, ROUND_UP = 19;

    // Totals over a collection's rows.
    public static final int SUM = 0, SMALLEST = 1, LARGEST = 2, COUNT = 3;

    private byte[] bytes = new byte[64];
    private int length = 0;

    private void room(int more) {
        if (length + more > bytes.length) {
            bytes = Arrays.copyOf(bytes, Math.max(bytes.length * 2, length + more));
        }
    }

    public Wire put(byte value) {
        room(1);
        bytes[length++] = value;
        return this;
    }

    public Wire put(int value) {
        room(4);
        for (int shift = 0; shift < 32; shift += 8) {
            bytes[length++] = (byte) (value >>> shift);
        }
        return this;
    }

    public Wire put(long value) {
        room(8);
        for (int shift = 0; shift < 64; shift += 8) {
            bytes[length++] = (byte) (value >>> shift);
        }
        return this;
    }

    public Wire put(byte[] more) {
        room(more.length);
        System.arraycopy(more, 0, bytes, length, more.length);
        length += more.length;
        return this;
    }

    public Wire put(String text) {
        byte[] utf8 = text.getBytes(StandardCharsets.UTF_8);
        return put(utf8.length).put(utf8);
    }

    /** A value: a Double, a Long, a BigDecimal, a Boolean or a String. */
    public Wire value(Object value) {
        switch (value) {
            case Double number -> put(NUMBER).put(Double.doubleToRawLongBits(number));
            case Long whole -> put(WHOLE).put(whole.longValue());
            case BigDecimal decimal -> {
                long[] parts = parts(decimal);
                put(DECIMAL).put(parts[0]).put(parts[1]).put((int) parts[2]);
            }
            case Boolean flag -> put(FLAG).put((byte) (flag ? 1 : 0));
            case String text -> put(TEXT).put(text);
            case null, default -> throw new WatershedException(
                    "a value is a Double, a Long, a BigDecimal, a Boolean or a String, not " + value);
        }
        return this;
    }

    public byte[] bytes() {
        return Arrays.copyOf(bytes, length);
    }

    // ------------------------------------------------------------- decimals

    /** A decimal as the engine takes it: its digits as one whole number in two halves, and its scale. */
    static long[] parts(BigDecimal decimal) {
        if (decimal.scale() < 0) {
            decimal = decimal.setScale(0);
        }
        BigInteger digits = decimal.unscaledValue();
        if (digits.bitLength() > 96 || decimal.scale() > 28) {
            throw new WatershedException(decimal.toPlainString()
                    + " has more digits than the engine holds: it keeps 28 significant digits");
        }
        return new long[] {digits.longValue(), digits.shiftRight(64).longValue(), decimal.scale()};
    }

    static BigDecimal decimal(long low, long high, int scale) {
        BigInteger digits = BigInteger.valueOf(high).shiftLeft(64)
                .or(new BigInteger(Long.toUnsignedString(low)));
        return new BigDecimal(digits, scale);
    }

    // ---------------------------------------------------------- expressions
    //
    // An expression is the steps a calculator with a stack would take:
    // `qty * price + 1` is qty, price, multiply, 1, add. So an operation is
    // its operands' steps, then one more.

    public static byte[] node(int node) {
        return new Wire().put((byte) 0).put(node).bytes();
    }

    public static byte[] part(String name) {
        return new Wire().put((byte) 1).put(name).bytes();
    }

    public static byte[] constant(Object value) {
        return new Wire().put((byte) 2).value(value).bytes();
    }

    public static byte[] unary(byte op, byte[] a) {
        return new Wire().put(a).put((byte) 3).put(op).bytes();
    }

    public static byte[] binary(byte op, byte[] a, byte[] b) {
        return new Wire().put(a).put(b).put((byte) 4).put(op).bytes();
    }

    public static byte[] choice(byte[] condition, byte[] then, byte[] otherwise) {
        return new Wire().put(condition).put(then).put(otherwise).put((byte) 5).bytes();
    }

    public static byte[] choose(byte[] index, List<byte[]> options) {
        Wire wire = new Wire().put(index);
        options.forEach(wire::put);
        return wire.put((byte) 6).put(options.size()).bytes();
    }

    public static byte[] texts(List<byte[]> items) {
        Wire wire = new Wire();
        items.forEach(wire::put);
        return wire.put((byte) 7).put(items.size()).bytes();
    }
}
