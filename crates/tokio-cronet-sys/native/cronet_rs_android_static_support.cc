// Committed wrapper for static Android embedding.

#include <malloc.h>
#include <stdlib.h>

#include "base/android/jni_android.h"

extern "C" __attribute__((visibility("default"))) jint
Cronet_RS_InitializeJavaVM(JavaVM* vm) {
  base::android::InitVM(vm);
  return JNI_VERSION_1_6;
}

// Chromium's normal Android executable link uses --wrap for its allocator.
// A Rust static dependency must not interpose the application's allocator, so
// satisfy the fallback dispatch with direct bionic calls instead.
extern "C" void* __real_malloc(size_t size) { return malloc(size); }
extern "C" void* __real_calloc(size_t count, size_t size) {
  return calloc(count, size);
}
extern "C" void* __real_realloc(void* address, size_t size) {
  return realloc(address, size);
}
extern "C" void __real_free(void* address) { free(address); }
extern "C" void* __real_memalign(size_t alignment, size_t size) {
  return memalign(alignment, size);
}
extern "C" size_t __real_malloc_usable_size(void* address) {
  return malloc_usable_size(address);
}
