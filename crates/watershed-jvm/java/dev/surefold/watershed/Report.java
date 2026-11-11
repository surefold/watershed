package dev.surefold.watershed;

/**
 * What one update did.
 *
 * @param update        this update's number; the first is 1
 * @param inputsChanged inputs whose value actually changed
 * @param recalculated  calculated nodes that were recalculated
 * @param changed       of those, how many came out with a different value
 */
public record Report(long update, long inputsChanged, long recalculated, long changed) {}
