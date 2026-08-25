// Link-time adapters for APIs that Chromium's Linux graph expects but the
// OpenHarmony Native SDK does not provide. Keeping these definitions beside
// Cronet avoids patching the corresponding upstream translation units.

#include <ostream>
#include <utility>

#include "base/debug/stack_trace.h"
#include "base/nix/xdg_util.h"
#include "net/cert/internal/system_trust_store.h"
#include "net/cert/internal/trust_store_chrome.h"

namespace base {

#if !defined(HAVE_BACKTRACE)
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
