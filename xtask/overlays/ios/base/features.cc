// Compile upstream base/features.cc with the Blink-compatible Apple message
// pump selection required by native Cronet on iOS. This wrapper changes only
// the build flag seen by this translation unit.
#include "build/blink_buildflags.h"

#undef BUILDFLAG_INTERNAL_USE_BLINK
#define BUILDFLAG_INTERNAL_USE_BLINK() (1)

#include "base/features_upstream.cc"
