// OHOS cannot resolve RTLD_NEXT close() reliably when a static Cronet archive
// is embedded in a shared object. Compile the upstream ownership bookkeeping
// through its component-build branch, which omits only that interposer.
#if defined(__OHOS__) && !defined(COMPONENT_BUILD)
#define COMPONENT_BUILD
#define CRONET_RS_RESTORE_COMPONENT_BUILD
#endif

#include "base/files/scoped_file_linux_upstream.cc"

#if defined(CRONET_RS_RESTORE_COMPONENT_BUILD)
#undef CRONET_RS_RESTORE_COMPONENT_BUILD
#undef COMPONENT_BUILD
#endif
