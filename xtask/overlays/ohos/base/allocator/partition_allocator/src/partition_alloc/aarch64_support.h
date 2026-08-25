// The OHOS Native SDK has no glibc sys/ifunc.h. Reuse upstream's implementation
// while making this one header observe the actual platform capability.
#include "partition_alloc/build_config.h"

#if defined(__OHOS__)
#undef PA_BUILDFLAG_INTERNAL_IS_LINUX
#define PA_BUILDFLAG_INTERNAL_IS_LINUX() (0)
#define CRONET_RS_RESTORE_PA_IS_LINUX
#endif

#include "partition_alloc/aarch64_support_upstream.h"

#if defined(CRONET_RS_RESTORE_PA_IS_LINUX)
#undef CRONET_RS_RESTORE_PA_IS_LINUX
#undef PA_BUILDFLAG_INTERNAL_IS_LINUX
#define PA_BUILDFLAG_INTERNAL_IS_LINUX() (1)
#endif
