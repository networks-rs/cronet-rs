// Wrapper-only network-handle table. Upstream Cronet_UrlRequest has no bind
// API; WebSocket connections created by cronet-rs read this table and pass the
// handle to CronetContext::GetURLRequestContext on the network thread.

#include <map>

#include "base/command_line.h"
#include "base/synchronization/lock.h"
#include "components/cronet_rs/cronet_rs_c.h"

namespace {

base::Lock g_lock;
std::map<Cronet_EnginePtr, int64_t> g_bound_networks;

}  // namespace

CRONET_EXPORT Cronet_RESULT
Cronet_RS_Engine_StartWithParams(Cronet_EnginePtr engine,
                                 Cronet_EngineParamsPtr params) {
  {
    base::AutoLock lock(g_lock);
    if (!base::CommandLine::InitializedForCurrentProcess()) {
      base::CommandLine::Init(0, nullptr);
    }
  }
  return Cronet_Engine_StartWithParams(engine, params);
}

CRONET_EXPORT void Cronet_RS_Engine_BindToNetwork(Cronet_EnginePtr engine,
                                                  int64_t network_handle) {
  base::AutoLock lock(g_lock);
  if (engine == nullptr || network_handle == CRONET_RS_UNBIND_NETWORK_HANDLE) {
    g_bound_networks.erase(engine);
    return;
  }
  g_bound_networks[engine] = network_handle;
}

CRONET_EXPORT int64_t
Cronet_RS_Engine_GetBoundNetwork(Cronet_EnginePtr engine) {
  base::AutoLock lock(g_lock);
  auto it = g_bound_networks.find(engine);
  if (it == g_bound_networks.end()) {
    return CRONET_RS_UNBIND_NETWORK_HANDLE;
  }
  return it->second;
}

CRONET_EXPORT void Cronet_RS_Engine_ClearBoundNetwork(Cronet_EnginePtr engine) {
  Cronet_RS_Engine_BindToNetwork(engine, CRONET_RS_UNBIND_NETWORK_HANDLE);
}
