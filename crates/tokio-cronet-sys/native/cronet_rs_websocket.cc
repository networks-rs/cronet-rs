// WebSocketChannel adapter compiled into Cronet by the overlay GN target.
// It does not modify upstream Cronet sources.

#include "components/cronet_rs/cronet_rs_c.h"
#include "components/cronet/native/include/cronet_c.h"
#include "net/base/network_handle.h"

#include <stdint.h>

#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "base/functional/bind.h"
#include "base/location.h"
#include "base/memory/raw_ptr.h"
#include "base/memory/scoped_refptr.h"
#include "base/strings/string_split.h"
#include "base/synchronization/lock.h"
#include "base/synchronization/waitable_event.h"
#include "base/thread_annotations.h"
#include "components/cronet/cronet_context.h"
#include "components/cronet/native/engine.h"
#include "net/base/auth.h"
#include "net/base/io_buffer.h"
#include "net/base/isolation_info.h"
#include "net/base/net_errors.h"
#include "net/cookies/site_for_cookies.h"
#include "net/http/http_request_headers.h"
#include "net/storage_access_api/status.h"
#include "net/traffic_annotation/network_traffic_annotation.h"
#include "net/websockets/websocket_channel.h"
#include "net/websockets/websocket_event_interface.h"
#include "net/websockets/websocket_frame.h"
#include "net/websockets/websocket_handshake_response_info.h"
#include "url/gurl.h"
#include "url/origin.h"

namespace cronet {
namespace {

constexpr net::NetworkTrafficAnnotationTag kWebSocketTrafficAnnotation =
    net::DefineNetworkTrafficAnnotation("cronet_rs_websocket", R"(
      semantics {
        sender: "Cronet"
        description: "WebSocket opened through the Cronet native C API."
        trigger: "An embedding application called Cronet_RS_WebSocket_Connect."
        data: "Application-provided handshake headers and WebSocket frames."
        destination: OTHER
      }
      policy {
        cookies_allowed: YES
        cookies_store: "user"
        setting: "This request is initiated by the embedding application."
        policy_exception_justification:
            "The embedding application is responsible for user settings."
      })");

class Cronet_WebSocketImpl;

class WebSocketEventHandler : public net::WebSocketEventInterface {
 public:
  explicit WebSocketEventHandler(Cronet_WebSocketImpl* owner) : owner_(owner) {}

  void OnCreateURLRequest(net::URLRequest* request) override;
  int OnURLRequestConnected(net::URLRequest* request,
                            const net::TransportInfo& info,
                            net::CompletionOnceCallback callback) override;
  void OnAddChannelResponse(
      std::unique_ptr<net::WebSocketHandshakeResponseInfo> response,
      const std::string& selected_subprotocol,
      const std::string& extensions) override;
  void OnDataFrame(bool fin,
                   WebSocketMessageType type,
                   base::span<const char> payload) override;
  bool HasPendingDataFrames() override;
  void OnSendDataFrameDone() override {}
  void OnClosingHandshake() override;
  void OnDropChannel(bool was_clean,
                     uint16_t code,
                     const std::string& reason) override;
  void OnFailChannel(const std::string& message,
                     int net_error,
                     std::optional<int> response_code) override;
  void OnStartOpeningHandshake(
      std::unique_ptr<net::WebSocketHandshakeRequestInfo> request) override {}
  void OnSSLCertificateError(
      std::unique_ptr<SSLErrorCallbacks> ssl_error_callbacks,
      const GURL& url,
      int net_error,
      const net::SSLInfo& ssl_info,
      bool fatal) override;
  int OnAuthRequired(const net::AuthChallengeInfo& auth_info,
                     scoped_refptr<net::HttpResponseHeaders> response_headers,
                     const net::IPEndPoint& socket_address,
                     base::OnceCallback<void(const net::AuthCredentials*)>
                         callback,
                     std::optional<net::AuthCredentials>* credentials) override;

 private:
  const raw_ptr<Cronet_WebSocketImpl> owner_;
};

class Cronet_WebSocketImpl {
 public:
  explicit Cronet_WebSocketImpl(Cronet_EngineImpl* engine) : engine_(engine) {}

  Cronet_WebSocketImpl(const Cronet_WebSocketImpl&) = delete;
  Cronet_WebSocketImpl& operator=(const Cronet_WebSocketImpl&) = delete;

  ~Cronet_WebSocketImpl() { ResetChannelOnNetworkThreadIfNeeded(); }

  void SetCallbacks(Cronet_ClientContext context,
                    Cronet_RS_WebSocket_OnOpen on_open,
                    Cronet_RS_WebSocket_OnMessage on_message,
                    Cronet_RS_WebSocket_OnClosing on_closing,
                    Cronet_RS_WebSocket_OnClosed on_closed,
                    Cronet_RS_WebSocket_OnFailure on_failure) {
    base::AutoLock lock(lock_);
    client_context_ = context;
    on_open_ = on_open;
    on_message_ = on_message;
    on_closing_ = on_closing;
    on_closed_ = on_closed;
    on_failure_ = on_failure;
  }

  void AddHeader(const std::string& name, const std::string& value) {
    base::AutoLock lock(lock_);
    extra_headers_.SetHeader(name, value);
  }

  void Connect(const std::string& url,
               const std::string& origin,
               const std::string& protocols) {
    if (!engine_ || !engine_->cronet_url_request_context()) {
      NotifyFailure("engine is not started", net::ERR_UNEXPECTED);
      return;
    }
    engine_->GetBidirectionalStreamEngine();
    engine_->cronet_url_request_context()->PostTaskToNetworkThread(
        FROM_HERE,
        base::BindOnce(&Cronet_WebSocketImpl::ConnectOnNetworkThread,
                       base::Unretained(this), url, origin, protocols));
  }

  void Send(bool binary, std::string payload) {
    CronetContext* context =
        engine_ ? engine_->cronet_url_request_context() : nullptr;
    if (!context) {
      NotifyFailure("engine is not started", net::ERR_UNEXPECTED);
      return;
    }
    context->PostTaskToNetworkThread(
        FROM_HERE, base::BindOnce(&Cronet_WebSocketImpl::SendOnNetworkThread,
                                  base::Unretained(this), binary,
                                  std::move(payload)));
  }

  void Close(uint16_t code, std::string reason) {
    CronetContext* context =
        engine_ ? engine_->cronet_url_request_context() : nullptr;
    if (!context) {
      return;
    }
    context->PostTaskToNetworkThread(
        FROM_HERE, base::BindOnce(&Cronet_WebSocketImpl::CloseOnNetworkThread,
                                  base::Unretained(this), code,
                                  std::move(reason)));
  }

  void DestroyAndWait() {
    CronetContext* context =
        engine_ ? engine_->cronet_url_request_context() : nullptr;
    if (!context || context->IsOnNetworkThread()) {
      ResetChannel();
      return;
    }
    destroyed_.Reset();
    context->PostTaskToNetworkThread(
        FROM_HERE,
        base::BindOnce(&Cronet_WebSocketImpl::DestroyOnNetworkThread,
                       base::Unretained(this)));
    destroyed_.Wait();
  }

  net::WebSocketChannel* channel() { return channel_.get(); }

  void NotifyOpen(const std::string& protocol) {
    Cronet_RS_WebSocket_OnOpen callback;
    Cronet_ClientContext context;
    {
      base::AutoLock lock(lock_);
      callback = on_open_;
      context = client_context_;
    }
    if (callback) {
      callback(context, reinterpret_cast<Cronet_RS_WebSocketPtr>(this),
               protocol.c_str());
    }
  }

  void NotifyMessage(bool binary, std::string payload) {
    Cronet_RS_WebSocket_OnMessage callback;
    Cronet_ClientContext context;
    {
      base::AutoLock lock(lock_);
      callback = on_message_;
      context = client_context_;
    }
    if (callback) {
      callback(context, reinterpret_cast<Cronet_RS_WebSocketPtr>(this),
               payload.data(), payload.size(), binary);
    }
  }

  void NotifyClosing() {
    Cronet_RS_WebSocket_OnClosing callback;
    Cronet_ClientContext context;
    {
      base::AutoLock lock(lock_);
      callback = on_closing_;
      context = client_context_;
    }
    if (callback) {
      callback(context, reinterpret_cast<Cronet_RS_WebSocketPtr>(this));
    }
  }

  void NotifyClosed(bool was_clean, uint16_t code, const std::string& reason) {
    Cronet_RS_WebSocket_OnClosed callback;
    Cronet_ClientContext context;
    {
      base::AutoLock lock(lock_);
      callback = on_closed_;
      context = client_context_;
    }
    if (callback) {
      callback(context, reinterpret_cast<Cronet_RS_WebSocketPtr>(this),
               was_clean, code, reason.c_str());
    }
  }

  void NotifyFailure(const std::string& message, int net_error) {
    Cronet_RS_WebSocket_OnFailure callback;
    Cronet_ClientContext context;
    {
      base::AutoLock lock(lock_);
      callback = on_failure_;
      context = client_context_;
    }
    if (callback) {
      callback(context, reinterpret_cast<Cronet_RS_WebSocketPtr>(this),
               message.c_str(), net_error);
    }
  }

  void ResetChannel() { channel_.reset(); }

  void AppendFrame(bool fin, int type, base::span<const char> payload) {
    if (pending_payload_.empty()) {
      pending_binary_ = type == net::WebSocketFrameHeader::kOpCodeBinary;
    }
    pending_payload_.append(payload.data(), payload.size());
    if (fin) {
      std::string payload_copy = std::move(pending_payload_);
      pending_payload_.clear();
      NotifyMessage(pending_binary_, std::move(payload_copy));
    }
  }

 private:
  void ConnectOnNetworkThread(std::string url,
                              std::string origin,
                              std::string protocols) {
    GURL socket_url(url);
    if (!socket_url.is_valid() || !socket_url.SchemeIsWSOrWSS()) {
      NotifyFailure("WebSocket URL must use ws or wss",
                    net::ERR_INVALID_URL);
      return;
    }
    url::Origin request_origin = origin.empty()
                                     ? url::Origin::Create(socket_url)
                                     : url::Origin::Create(GURL(origin));
    auto event = std::make_unique<WebSocketEventHandler>(this);
    net::URLRequestContext* request_context =
        engine_->cronet_url_request_context()->GetURLRequestContext(
            static_cast<net::handles::NetworkHandle>(
                Cronet_RS_Engine_GetBoundNetwork(engine_)));
    if (!request_context) {
      NotifyFailure("URLRequestContext is unavailable", net::ERR_UNEXPECTED);
      return;
    }
    channel_ = std::make_unique<net::WebSocketChannel>(std::move(event),
                                                       request_context);
    std::vector<std::string> requested_protocols;
    if (!protocols.empty()) {
      requested_protocols = base::SplitString(
          protocols, ",", base::TRIM_WHITESPACE, base::SPLIT_WANT_NONEMPTY);
    }
    net::HttpRequestHeaders headers;
    {
      base::AutoLock lock(lock_);
      headers = extra_headers_;
    }
    channel_->SendAddChannelRequest(
        socket_url, requested_protocols, request_origin,
        net::SiteForCookies::FromOrigin(request_origin),
        net::StorageAccessApiStatus::kNone,
        net::IsolationInfo::CreateForInternalRequest(request_origin), headers,
        kWebSocketTrafficAnnotation);
  }

  void SendOnNetworkThread(bool binary, std::string payload) {
    if (!channel_) {
      NotifyFailure("WebSocket is not connected", net::ERR_CONNECTION_CLOSED);
      return;
    }
    std::vector<uint8_t> bytes(payload.begin(), payload.end());
    auto buffer =
        base::MakeRefCounted<net::VectorIOBuffer>(std::move(bytes));
    net::WebSocketFrameHeader::OpCode opcode =
        binary ? net::WebSocketFrameHeader::kOpCodeBinary
               : net::WebSocketFrameHeader::kOpCodeText;
    if (channel_->SendFrame(true, opcode, buffer, payload.size()) ==
        net::WebSocketChannel::CHANNEL_DELETED) {
      return;
    }
  }

  void CloseOnNetworkThread(uint16_t code, std::string reason) {
    if (!channel_) {
      return;
    }
    if (channel_->StartClosingHandshake(code, reason) ==
        net::WebSocketChannel::CHANNEL_DELETED) {
      return;
    }
  }

  void DestroyOnNetworkThread() {
    ResetChannel();
    destroyed_.Signal();
  }

  void ResetChannelOnNetworkThreadIfNeeded() {
    CronetContext* context =
        engine_ ? engine_->cronet_url_request_context() : nullptr;
    if (!context || context->IsOnNetworkThread()) {
      ResetChannel();
      return;
    }
    DestroyAndWait();
  }

  const raw_ptr<Cronet_EngineImpl> engine_;
  std::unique_ptr<net::WebSocketChannel> channel_;
  std::string pending_payload_;
  bool pending_binary_ = false;
  base::Lock lock_;
  Cronet_ClientContext client_context_ GUARDED_BY(lock_) = nullptr;
  Cronet_RS_WebSocket_OnOpen on_open_ GUARDED_BY(lock_) = nullptr;
  Cronet_RS_WebSocket_OnMessage on_message_ GUARDED_BY(lock_) = nullptr;
  Cronet_RS_WebSocket_OnClosing on_closing_ GUARDED_BY(lock_) = nullptr;
  Cronet_RS_WebSocket_OnClosed on_closed_ GUARDED_BY(lock_) = nullptr;
  Cronet_RS_WebSocket_OnFailure on_failure_ GUARDED_BY(lock_) = nullptr;
  net::HttpRequestHeaders extra_headers_ GUARDED_BY(lock_);
  base::WaitableEvent destroyed_{
      base::WaitableEvent::ResetPolicy::MANUAL,
      base::WaitableEvent::InitialState::NOT_SIGNALED};
};

void WebSocketEventHandler::OnCreateURLRequest(net::URLRequest*) {}

int WebSocketEventHandler::OnURLRequestConnected(
    net::URLRequest*,
    const net::TransportInfo&,
    net::CompletionOnceCallback) {
  return net::OK;
}

void WebSocketEventHandler::OnAddChannelResponse(
    std::unique_ptr<net::WebSocketHandshakeResponseInfo>,
    const std::string& selected_subprotocol,
    const std::string&) {
  owner_->NotifyOpen(selected_subprotocol);
  net::WebSocketChannel* channel = owner_->channel();
  if (!channel) {
    return;
  }
  // OnConnectSuccess does not start reading. Call ReadFrames once after the
  // handshake; the channel then continues on its own read loop.
  if (channel->ReadFrames() == net::WebSocketChannel::CHANNEL_DELETED) {
    return;
  }
}

void WebSocketEventHandler::OnDataFrame(bool fin,
                                        WebSocketMessageType type,
                                        base::span<const char> payload) {
  // Copy immediately: |payload| is invalidated by the next ReadFrames call.
  // Do not call ReadFrames here — it is already on the stack and DCHECKs
  // that read_frames_ is empty.
  owner_->AppendFrame(fin, type, payload);
}

bool WebSocketEventHandler::HasPendingDataFrames() {
  return false;
}

void WebSocketEventHandler::OnClosingHandshake() {
  owner_->NotifyClosing();
}

void WebSocketEventHandler::OnDropChannel(bool was_clean,
                                          uint16_t code,
                                          const std::string& reason) {
  Cronet_WebSocketImpl* owner = owner_.get();
  owner->NotifyClosed(was_clean, code, reason);
  owner->ResetChannel();
}

void WebSocketEventHandler::OnFailChannel(const std::string& message,
                                          int net_error,
                                          std::optional<int>) {
  Cronet_WebSocketImpl* owner = owner_.get();
  owner->NotifyFailure(message, net_error);
  owner->ResetChannel();
}

void WebSocketEventHandler::OnSSLCertificateError(
    std::unique_ptr<SSLErrorCallbacks> ssl_error_callbacks,
    const GURL&,
    int,
    const net::SSLInfo&,
    bool) {
  ssl_error_callbacks->CancelSSLRequest(net::ERR_CERT_INVALID, nullptr);
}

int WebSocketEventHandler::OnAuthRequired(
    const net::AuthChallengeInfo&,
    scoped_refptr<net::HttpResponseHeaders>,
    const net::IPEndPoint&,
    base::OnceCallback<void(const net::AuthCredentials*)>,
    std::optional<net::AuthCredentials>* credentials) {
  *credentials = std::nullopt;
  return net::OK;
}

}  // namespace
}  // namespace cronet

CRONET_EXPORT Cronet_RS_WebSocketPtr Cronet_RS_WebSocket_Create(
    Cronet_EnginePtr engine) {
  return reinterpret_cast<Cronet_RS_WebSocketPtr>(new cronet::Cronet_WebSocketImpl(
      static_cast<cronet::Cronet_EngineImpl*>(engine)));
}

CRONET_EXPORT void Cronet_RS_WebSocket_Destroy(Cronet_RS_WebSocketPtr websocket) {
  auto* impl = reinterpret_cast<cronet::Cronet_WebSocketImpl*>(websocket);
  impl->DestroyAndWait();
  delete impl;
}

CRONET_EXPORT void Cronet_RS_WebSocket_SetCallbacks(
    Cronet_RS_WebSocketPtr websocket,
    Cronet_ClientContext context,
    Cronet_RS_WebSocket_OnOpen on_open,
    Cronet_RS_WebSocket_OnMessage on_message,
    Cronet_RS_WebSocket_OnClosing on_closing,
    Cronet_RS_WebSocket_OnClosed on_closed,
    Cronet_RS_WebSocket_OnFailure on_failure) {
  reinterpret_cast<cronet::Cronet_WebSocketImpl*>(websocket)->SetCallbacks(
      context, on_open, on_message, on_closing, on_closed, on_failure);
}

CRONET_EXPORT void Cronet_RS_WebSocket_AddHeader(Cronet_RS_WebSocketPtr websocket,
                                              Cronet_String name,
                                              Cronet_String value) {
  reinterpret_cast<cronet::Cronet_WebSocketImpl*>(websocket)->AddHeader(
      name ? name : "", value ? value : "");
}

CRONET_EXPORT void Cronet_RS_WebSocket_Connect(Cronet_RS_WebSocketPtr websocket,
                                            Cronet_String url,
                                            Cronet_String origin,
                                            Cronet_String protocols) {
  reinterpret_cast<cronet::Cronet_WebSocketImpl*>(websocket)->Connect(
      url ? url : "", origin ? origin : "", protocols ? protocols : "");
}

CRONET_EXPORT void Cronet_RS_WebSocket_Send(Cronet_RS_WebSocketPtr websocket,
                                         const char* data,
                                         uint64_t length,
                                         bool binary) {
  std::string payload(data ? data : "", static_cast<size_t>(length));
  reinterpret_cast<cronet::Cronet_WebSocketImpl*>(websocket)->Send(
      binary, std::move(payload));
}

CRONET_EXPORT void Cronet_RS_WebSocket_Close(Cronet_RS_WebSocketPtr websocket,
                                          uint16_t code,
                                          Cronet_String reason) {
  reinterpret_cast<cronet::Cronet_WebSocketImpl*>(websocket)->Close(
      code, reason ? reason : "");
}
