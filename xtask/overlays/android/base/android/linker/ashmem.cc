// Android API levels below 26 hide __system_property_read_callback from the
// NDK headers. Chromium already has a runtime availability check, so map that
// call to an equivalent operation built from the older property API.

#include <sys/system_properties.h>

#include <cstdint>

namespace {

void CronetRsSystemPropertyReadCallback(
    const prop_info* info,
    void (*callback)(void*, const char*, const char*, uint32_t),
    void* cookie) {
  char name[PROP_NAME_MAX] = {};
  char value[PROP_VALUE_MAX] = {};
  if (__system_property_read(info, name, value) >= 0) {
    callback(cookie, name, value, 0);
  }
}

}  // namespace

#define __system_property_read_callback CronetRsSystemPropertyReadCallback
#include "base/android/linker/ashmem_upstream.cc"
#undef __system_property_read_callback
