// Committed wrapper. The C API needs Chromium's JavaVM and application
// ClassLoader bridge on Android, but not the Cronet Java API.

#include "base/android/jni_android.h"

extern "C" __attribute__((visibility("default"))) jint JNI_OnLoad(JavaVM* vm,
                                                                  void*) {
  base::android::InitVM(vm);
  return JNI_VERSION_1_6;
}
