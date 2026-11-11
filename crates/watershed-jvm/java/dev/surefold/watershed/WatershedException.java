package dev.surefold.watershed;

/** Anything the engine refuses, with its reason. */
public class WatershedException extends RuntimeException {
    public WatershedException(String reason) {
        super(reason);
    }
}
