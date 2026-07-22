package io.github.southorange.cronet.e2e;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import org.chromium.base.ContextUtils;

public final class MainActivity extends Activity {
    private static final String TAG = "cronet-rs-e2e";

    private native int runCronetE2e();

    private void loadNativeLibraries() {
        // Load the versioned Cronet library explicitly so Android invokes its
        // JNI_OnLoad before the Rust library starts the native engine. Discover
        // the pinned version from the packaged file instead of duplicating it.
        File nativeDirectory = new File(getApplicationInfo().nativeLibraryDir);
        File[] libraries = nativeDirectory.listFiles();
        if (libraries == null) {
            throw new IllegalStateException("native library directory is unavailable");
        }
        for (File library : libraries) {
            if (library.getName().startsWith("libcronet.") && library.getName().endsWith(".so")) {
                System.load(library.getAbsolutePath());
                System.loadLibrary("cronet_mobile_e2e_runner");
                return;
            }
        }
        // With the `static` feature Cronet is linked into the Rust runner. Its
        // JNI_OnLoad is retained by whole-archive linking and runs here.
        System.loadLibrary("cronet_mobile_e2e_runner");
    }

    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
        // Native Cronet's Android proxy, certificate and connectivity bridges
        // obtain the application context through Chromium base.
        ContextUtils.initApplicationContext(getApplicationContext());
        loadNativeLibraries();
        new Thread(() -> {
            int result = runCronetE2e();
            String text = result == 0 ? "PASS\n" : "FAIL: native runner returned " + result + "\n";
            try (FileOutputStream output = new FileOutputStream(
                    new File(getFilesDir(), "cronet-rs-e2e.txt"))) {
                output.write(text.getBytes(StandardCharsets.UTF_8));
            } catch (Exception error) {
                Log.e(TAG, "could not write result", error);
            }
            Log.i(TAG, "CRONET_RS_E2E_RESULT=" + text.trim());
        }, "cronet-rs-e2e").start();
    }
}
