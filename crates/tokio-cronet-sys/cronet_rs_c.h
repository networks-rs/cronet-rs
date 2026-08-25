// Extra C ABI implemented by cronet-rs wrapper sources. These symbols are not
// part of upstream cronet.idl; they are compiled beside Cronet_EngineImpl and
// only call already-public Cronet C++ methods.

#ifndef CRONET_RS_C_H_
#define CRONET_RS_C_H_

#include <stdint.h>

#include "cronet_export.h"
#include "cronet.idl_c.h"

#ifdef __cplusplus
extern "C" {
#endif

#define CRONET_RS_UNBIND_NETWORK_HANDLE ((int64_t)-1)

// Initializes the process-global state required by Chromium before delegating
// to the upstream Cronet C API. Native cronet-rs callers use this entry point
// so the upstream global-state source remains untouched.
CRONET_EXPORT Cronet_RESULT Cronet_RS_Engine_StartWithParams(
    Cronet_EnginePtr engine,
    Cronet_EngineParamsPtr params);

CRONET_EXPORT void Cronet_RS_Engine_BindToNetwork(Cronet_EnginePtr engine,
                                                  int64_t network_handle);
CRONET_EXPORT int64_t Cronet_RS_Engine_GetBoundNetwork(Cronet_EnginePtr engine);
CRONET_EXPORT void Cronet_RS_Engine_ClearBoundNetwork(Cronet_EnginePtr engine);

typedef struct Cronet_RS_WebSocket Cronet_RS_WebSocket;
typedef struct Cronet_RS_WebSocket* Cronet_RS_WebSocketPtr;

typedef void (*Cronet_RS_WebSocket_OnOpen)(Cronet_ClientContext context,
                                           Cronet_RS_WebSocketPtr websocket,
                                           const char* protocol);
typedef void (*Cronet_RS_WebSocket_OnMessage)(Cronet_ClientContext context,
                                              Cronet_RS_WebSocketPtr websocket,
                                              const char* data,
                                              uint64_t length,
                                              bool binary);
typedef void (*Cronet_RS_WebSocket_OnClosing)(Cronet_ClientContext context,
                                              Cronet_RS_WebSocketPtr websocket);
typedef void (*Cronet_RS_WebSocket_OnClosed)(Cronet_ClientContext context,
                                             Cronet_RS_WebSocketPtr websocket,
                                             bool was_clean,
                                             uint16_t code,
                                             const char* reason);
typedef void (*Cronet_RS_WebSocket_OnFailure)(Cronet_ClientContext context,
                                              Cronet_RS_WebSocketPtr websocket,
                                              const char* message,
                                              int net_error);

CRONET_EXPORT Cronet_RS_WebSocketPtr
Cronet_RS_WebSocket_Create(Cronet_EnginePtr engine);
CRONET_EXPORT void Cronet_RS_WebSocket_Destroy(Cronet_RS_WebSocketPtr websocket);
CRONET_EXPORT void Cronet_RS_WebSocket_SetCallbacks(
    Cronet_RS_WebSocketPtr websocket,
    Cronet_ClientContext context,
    Cronet_RS_WebSocket_OnOpen on_open,
    Cronet_RS_WebSocket_OnMessage on_message,
    Cronet_RS_WebSocket_OnClosing on_closing,
    Cronet_RS_WebSocket_OnClosed on_closed,
    Cronet_RS_WebSocket_OnFailure on_failure);
CRONET_EXPORT void Cronet_RS_WebSocket_AddHeader(
    Cronet_RS_WebSocketPtr websocket,
    Cronet_String name,
    Cronet_String value);
CRONET_EXPORT void Cronet_RS_WebSocket_Connect(Cronet_RS_WebSocketPtr websocket,
                                               Cronet_String url,
                                               Cronet_String origin,
                                               Cronet_String protocols);
CRONET_EXPORT void Cronet_RS_WebSocket_Send(Cronet_RS_WebSocketPtr websocket,
                                            const char* data,
                                            uint64_t length,
                                            bool binary);
CRONET_EXPORT void Cronet_RS_WebSocket_Close(Cronet_RS_WebSocketPtr websocket,
                                             uint16_t code,
                                             Cronet_String reason);

#ifdef __cplusplus
}
#endif

#endif  // CRONET_RS_C_H_
