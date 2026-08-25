// OpenHarmony does not expose Linux/glibc's process-title machinery.
#include "base/process/set_process_title.h"

namespace base {

void SetProcessTitleFromCommandLine(const char**) {}

}  // namespace base
