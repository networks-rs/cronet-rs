package internal.org.jni_zero;

import java.util.Collections;

/** Minimal runtime support consumed by Chromium's generated JNI bridge. */
public final class JniInit {
    private JniInit() {}

    private static Object[] init() {
        return new Object[] {Collections.EMPTY_LIST, Collections.EMPTY_MAP};
    }

    private static void crashIfMultiplexingMisaligned(long wholeHash, long priorityHash) {
        // The native C API does not package or invoke multiplexed Java natives.
    }
}
