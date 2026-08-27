// Link-time adapters for standalone targets using Chromium's Linux graph.
// Keeping these definitions beside Cronet avoids patching upstream sources.

#include <utility>

#include "base/nix/xdg_util.h"
#include "net/cert/internal/system_trust_store.h"
#include "net/cert/internal/trust_store_chrome.h"

#if defined(CRONET_TARGET_OHOS)
#include <ostream>

#include "base/debug/stack_trace.h"
#endif

namespace base {

#if defined(CRONET_TARGET_OHOS) && !defined(HAVE_BACKTRACE)
void debug::StackTrace::OutputToStreamWithPrefixImpl(
    std::ostream* output,
    cstring_view prefix) const {
  for (const void* address : addresses()) {
    *output << prefix << address << '\n';
  }
}
#endif

namespace nix {

DesktopEnvironment GetDesktopEnvironment(Environment*) {
  return DESKTOP_ENVIRONMENT_OTHER;
}

}  // namespace nix
}  // namespace base

namespace net {

#if BUILDFLAG(CHROME_ROOT_STORE_SUPPORTED)
std::unique_ptr<SystemTrustStore> CreateSslSystemTrustStoreChromeRoot(
    std::unique_ptr<TrustStoreChrome> chrome_root) {
  return CreateChromeOnlySystemTrustStore(std::move(chrome_root));
}
#endif

}  // namespace net
