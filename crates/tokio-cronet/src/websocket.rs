//! WebSocket client over Chromium's `WebSocketChannel`.

use std::{
    ffi::{CStr, c_void},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    Engine, Error, Header, Result,
    engine::{EngineOperation, RequestCanceler},
    types::{to_cstring, validate_string},
};

/// A WebSocket text or binary message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Bytes),
}

/// Builder for a Cronet WebSocket connection.
#[derive(Debug)]
pub struct WebSocketBuilder {
    url: String,
    origin: Option<String>,
    protocols: Vec<String>,
    headers: Vec<Header>,
}

impl WebSocketBuilder {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        validate_string(&url, "WebSocket URL")?;
        Ok(Self {
            url,
            origin: None,
            protocols: Vec::new(),
            headers: Vec::new(),
        })
    }

    pub fn origin(mut self, origin: impl Into<String>) -> Result<Self> {
        let origin = origin.into();
        validate_string(&origin, "WebSocket origin")?;
        self.origin = Some(origin);
        Ok(self)
    }

    pub fn protocol(mut self, protocol: impl Into<String>) -> Result<Self> {
        let protocol = protocol.into();
        validate_string(&protocol, "WebSocket protocol")?;
        self.protocols.push(protocol);
        Ok(self)
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        self.headers.push(Header::new(name, value)?);
        Ok(self)
    }

    pub async fn open(self, engine: &Engine) -> Result<WebSocket> {
        open_websocket(engine, self).await
    }
}

/// A connected WebSocket.
pub struct WebSocket {
    native: usize,
    messages: mpsc::Receiver<Result<WsMessage>>,
    protocol: String,
    terminal: watch::Receiver<Option<Result<()>>>,
    context_ptr: *const WsContext,
    context: Arc<WsContext>,
}

impl WebSocket {
    pub fn builder(url: impl Into<String>) -> Result<WebSocketBuilder> {
        WebSocketBuilder::new(url)
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    #[allow(clippy::unused_async)]
    pub async fn send_text(&self, text: impl Into<String>) -> Result<()> {
        let text = text.into();
        self.send(text.as_bytes(), false)
    }

    #[allow(clippy::unused_async)]
    pub async fn send_binary(&self, payload: impl Into<Bytes>) -> Result<()> {
        let payload = payload.into();
        self.send(payload.as_ref(), true)
    }

    pub async fn next_message(&mut self) -> Option<Result<WsMessage>> {
        self.messages.recv().await
    }

    #[allow(clippy::unused_async)]
    pub async fn close(&self, code: u16, reason: impl Into<String>) -> Result<()> {
        let reason = to_cstring(&reason.into(), "WebSocket close reason")?;
        let native = self.native as sys::Cronet_RS_WebSocketPtr;
        // SAFETY: native remains owned until Drop.
        unsafe { sys::Cronet_RS_WebSocket_Close(native, code, reason.as_ptr()) };
        Ok(())
    }

    pub fn cancel(&self) {
        let native = self.native as sys::Cronet_RS_WebSocketPtr;
        if !native.is_null() {
            // SAFETY: cancel is equivalent to a close; Drop still destroys once.
            unsafe { sys::Cronet_RS_WebSocket_Close(native, 1001, ptr::null()) };
        }
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.terminal.borrow().is_some()
    }

    fn send(&self, payload: &[u8], binary: bool) -> Result<()> {
        if self.is_done() {
            return Err(Error::WebSocket {
                message: "WebSocket is closed".to_owned(),
                net_error: 0,
            });
        }
        let native = self.native as sys::Cronet_RS_WebSocketPtr;
        // SAFETY: payload is copied by the native wrapper before returning.
        unsafe {
            sys::Cronet_RS_WebSocket_Send(
                native,
                payload.as_ptr().cast(),
                u64::try_from(payload.len()).unwrap_or(u64::MAX),
                binary,
            );
        }
        Ok(())
    }
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        self.context.control.clear_native();
        let native = self.native as sys::Cronet_RS_WebSocketPtr;
        if !native.is_null() {
            // SAFETY: this is the unique owner of the native socket.
            unsafe { sys::Cronet_RS_WebSocket_Destroy(native) };
            self.native = 0;
        }
        if !self.context_ptr.is_null() {
            // SAFETY: extra strong count taken by Arc::into_raw in open_websocket.
            drop(unsafe { Arc::from_raw(self.context_ptr) });
            self.context_ptr = ptr::null();
        }
    }
}

use tokio_cronet_sys as sys;

// SAFETY: the raw pointer is only an extra Arc clone released in Drop after
// the native socket is destroyed. Message state is in Send channels.
unsafe impl Send for WebSocket {}

struct WsControl {
    native: AtomicUsize,
}

impl WsControl {
    fn set_native(&self, native: sys::Cronet_RS_WebSocketPtr) {
        self.native.store(native as usize, Ordering::Release);
    }

    fn clear_native(&self) {
        self.native.store(0, Ordering::Release);
    }

    fn cancel(&self) {
        let native = self.native.load(Ordering::Acquire) as sys::Cronet_RS_WebSocketPtr;
        if !native.is_null() {
            // SAFETY: native is live until Drop clears this pointer, then Destroy.
            unsafe { sys::Cronet_RS_WebSocket_Close(native, 1001, ptr::null()) };
        }
    }
}

impl RequestCanceler for WsControl {
    fn cancel(&self) {
        WsControl::cancel(self);
    }
}

struct WsContext {
    open: Mutex<Option<oneshot::Sender<Result<String>>>>,
    messages: mpsc::Sender<Result<WsMessage>>,
    terminal: watch::Sender<Option<Result<()>>>,
    operation: Mutex<Option<EngineOperation>>,
    control: Arc<WsControl>,
}

impl WsContext {
    fn finish(&self, result: Result<()>) {
        let _ = self.terminal.send(Some(result));
        let _ = self
            .operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

async fn open_websocket(engine: &Engine, builder: WebSocketBuilder) -> Result<WebSocket> {
    let operation = engine.inner.begin_operation()?;
    let engine_ptr = operation.raw();
    let url = to_cstring(&builder.url, "WebSocket URL")?;
    let origin = builder
        .origin
        .as_deref()
        .map(|origin| to_cstring(origin, "WebSocket origin"))
        .transpose()?;
    let protocols = to_cstring(&builder.protocols.join(","), "WebSocket protocol")?;
    let (open_tx, open_rx) = oneshot::channel();
    let (messages_tx, messages_rx) = mpsc::channel(32);
    let (terminal_tx, terminal_rx) = watch::channel(None);
    let control = Arc::new(WsControl {
        native: AtomicUsize::new(0),
    });
    let context = Arc::new(WsContext {
        open: Mutex::new(Some(open_tx)),
        messages: messages_tx,
        terminal: terminal_tx,
        operation: Mutex::new(Some(operation)),
        control: control.clone(),
    });
    // SAFETY: native constructor copies no borrowed state.
    let native = unsafe { sys::Cronet_RS_WebSocket_Create(engine_ptr) };
    if native.is_null() {
        return Err(Error::AllocationFailed("websocket"));
    }
    control.set_native(native);
    let canceler: Arc<dyn RequestCanceler> = control.clone();
    if engine.inner.register(&canceler) {
        control.clear_native();
        // SAFETY: register observed shutdown; the socket was never returned.
        unsafe { sys::Cronet_RS_WebSocket_Destroy(native) };
        return Err(Error::EngineShutdown);
    }
    let context_ptr = Arc::into_raw(context.clone());
    // SAFETY: callbacks copy data and post onto Tokio; context is released in Drop
    // after Destroy returns.
    unsafe {
        sys::Cronet_RS_WebSocket_SetCallbacks(
            native,
            context_ptr.cast::<c_void>().cast_mut(),
            Some(on_open),
            Some(on_message),
            Some(on_closing),
            Some(on_closed),
            Some(on_failure),
        );
        for header in &builder.headers {
            let name = header.c_name();
            let value = header.c_value();
            sys::Cronet_RS_WebSocket_AddHeader(native, name.as_ptr(), value.as_ptr());
        }
        sys::Cronet_RS_WebSocket_Connect(
            native,
            url.as_ptr(),
            origin
                .as_ref()
                .map_or(ptr::null(), |origin| origin.as_ptr()),
            protocols.as_ptr(),
        );
    }
    match open_rx.await {
        Ok(Ok(protocol)) => Ok(WebSocket {
            native: native as usize,
            messages: messages_rx,
            protocol,
            terminal: terminal_rx,
            context_ptr,
            context,
        }),
        Ok(Err(error)) => {
            control.clear_native();
            unsafe { sys::Cronet_RS_WebSocket_Destroy(native) };
            drop(unsafe { Arc::from_raw(context_ptr) });
            Err(error)
        }
        Err(_) => {
            control.clear_native();
            unsafe { sys::Cronet_RS_WebSocket_Destroy(native) };
            drop(unsafe { Arc::from_raw(context_ptr) });
            Err(Error::CallbackChannelClosed)
        }
    }
}

fn context_from(raw: sys::Cronet_ClientContext) -> Option<Arc<WsContext>> {
    if raw.is_null() {
        return None;
    }
    let raw = raw.cast::<WsContext>();
    // SAFETY: retained until Destroy; increment so callback can drop its clone.
    unsafe {
        Arc::increment_strong_count(raw);
        Some(Arc::from_raw(raw))
    }
}

unsafe extern "C" fn on_open(
    context: sys::Cronet_ClientContext,
    _websocket: sys::Cronet_RS_WebSocketPtr,
    protocol: *const std::ffi::c_char,
) {
    let Some(context) = context_from(context) else {
        return;
    };
    let protocol = if protocol.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(protocol) }
            .to_string_lossy()
            .into_owned()
    };
    if let Some(open) = context
        .open
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let _ = open.send(Ok(protocol));
    }
}

unsafe extern "C" fn on_message(
    context: sys::Cronet_ClientContext,
    _websocket: sys::Cronet_RS_WebSocketPtr,
    data: *const std::ffi::c_char,
    length: u64,
    binary: bool,
) {
    let Some(context) = context_from(context) else {
        return;
    };
    let bytes = if data.is_null() {
        Vec::new()
    } else {
        unsafe {
            std::slice::from_raw_parts(data.cast::<u8>(), usize::try_from(length).unwrap_or(0))
        }
        .to_vec()
    };
    let message = if binary {
        Ok(WsMessage::Binary(Bytes::from(bytes)))
    } else {
        Ok(WsMessage::Text(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))
    };
    let _ = context.messages.try_send(message);
}

unsafe extern "C" fn on_closing(
    _context: sys::Cronet_ClientContext,
    _websocket: sys::Cronet_RS_WebSocketPtr,
) {
}

unsafe extern "C" fn on_closed(
    context: sys::Cronet_ClientContext,
    _websocket: sys::Cronet_RS_WebSocketPtr,
    _was_clean: bool,
    _code: u16,
    _reason: *const std::ffi::c_char,
) {
    let Some(context) = context_from(context) else {
        return;
    };
    context.finish(Ok(()));
}

unsafe extern "C" fn on_failure(
    context: sys::Cronet_ClientContext,
    _websocket: sys::Cronet_RS_WebSocketPtr,
    message: *const std::ffi::c_char,
    net_error: i32,
) {
    let Some(context) = context_from(context) else {
        return;
    };
    let message = if message.is_null() {
        "WebSocket failed".to_owned()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    };
    let error = Error::WebSocket { message, net_error };
    if let Some(open) = context
        .open
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let _ = open.send(Err(error.clone()));
    }
    let _ = context.messages.try_send(Err(error.clone()));
    context.finish(Err(error));
}

impl Engine {
    pub fn websocket(&self, url: impl Into<String>) -> Result<WebSocketBuilder> {
        WebSocketBuilder::new(url)
    }

    pub async fn open_websocket(&self, builder: WebSocketBuilder) -> Result<WebSocket> {
        builder.open(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_rejects_interior_nul() {
        assert!(WebSocketBuilder::new("ws://example.com/\0").is_err());
        assert!(WebSocketBuilder::new("ws://127.0.0.1/ws").is_ok());
    }
}
