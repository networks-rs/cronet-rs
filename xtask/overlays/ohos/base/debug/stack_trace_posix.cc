// The OHOS Clang target advertises __GLIBC__ compatibility while its musl
// sysroot has no execinfo API. Reuse Chromium's implementation with its
// existing non-backtrace branch selected.

#if defined(__GLIBC__) && !defined(__UCLIBC__)
#define CRONET_RS_RESTORE_UCLIBC
#define __UCLIBC__ 1
#endif

#include "base/debug/stack_trace_posix_upstream.cc"

#if defined(CRONET_RS_RESTORE_UCLIBC)
#undef __UCLIBC__
#undef CRONET_RS_RESTORE_UCLIBC
#endif
