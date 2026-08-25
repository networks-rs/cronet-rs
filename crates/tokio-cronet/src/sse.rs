//! Server-Sent Events client over Cronet URL requests.

use std::time::Duration;

use bytes::Bytes;
use tokio::time::sleep;

use crate::{Engine, Header, Request, RequestBuilder, RequestHandle, ResponseBody, Result};

/// One dispatched SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub id: Option<String>,
    pub event: String,
    pub data: String,
}

/// Incremental `text/event-stream` parser.
#[derive(Debug, Default)]
struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data: Vec<String>,
    id: Option<String>,
    retry: Option<Duration>,
    last_id: Option<String>,
}

enum ParseOutput {
    Event(SseEvent),
    Retry(Duration),
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<ParseOutput> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.buffer.drain(..3);
        }
        let mut output = Vec::new();
        while let Some(end) = find_line_end(&self.buffer) {
            let line = self.buffer.drain(..end).collect::<Vec<_>>();
            let line = trim_line_ending(&line);
            if let Some(item) = self.push_line(line) {
                output.push(item);
            }
        }
        output
    }

    fn push_line(&mut self, line: &[u8]) -> Option<ParseOutput> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.first() == Some(&b':') {
            return None;
        }
        let (field, value) = split_field(line);
        match field {
            "event" => value.clone_into(&mut self.event),
            "data" => self.data.push(value.to_owned()),
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            "retry" => {
                if let Ok(millis) = value.parse::<u64>() {
                    let retry = Duration::from_millis(millis);
                    self.retry = Some(retry);
                    return Some(ParseOutput::Retry(retry));
                }
            }
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<ParseOutput> {
        if self.data.is_empty() {
            self.event.clear();
            self.id = None;
            return None;
        }
        let event = if self.event.is_empty() {
            "message".to_owned()
        } else {
            std::mem::take(&mut self.event)
        };
        let data = self.data.join("\n");
        self.data.clear();
        let id = self.id.take();
        if let Some(id) = &id {
            self.last_id = Some(id.clone());
        }
        Some(ParseOutput::Event(SseEvent { id, event, data }))
    }
}

fn find_line_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
        .map(|index| {
            if buffer[index] == b'\r' && buffer.get(index + 1) == Some(&b'\n') {
                index + 2
            } else {
                index + 1
            }
        })
}

fn trim_line_ending(line: &[u8]) -> &[u8] {
    match line {
        [rest @ .., b'\r', b'\n'] | [rest @ .., b'\n' | b'\r'] => rest,
        other => other,
    }
}

fn split_field(line: &[u8]) -> (&str, &str) {
    let text = std::str::from_utf8(line).unwrap_or("");
    match text.split_once(':') {
        Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
        None => (text, ""),
    }
}

/// Builder for a Cronet SSE request.
#[derive(Debug)]
pub struct EventSourceBuilder {
    request: RequestBuilder,
    last_event_id: Option<String>,
    auto_reconnect: bool,
    retry: Duration,
}

impl EventSourceBuilder {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        Ok(Self {
            request: Request::builder(url)?
                .method("GET")?
                .header("accept", "text/event-stream")?
                .header("cache-control", "no-cache")?
                .disable_cache(true)
                .max_response_bytes(usize::MAX),
            last_event_id: None,
            auto_reconnect: false,
            retry: Duration::from_secs(3),
        })
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        self.request = self.request.header(name, value)?;
        Ok(self)
    }

    #[must_use]
    pub fn last_event_id(mut self, id: impl Into<String>) -> Self {
        self.last_event_id = Some(id.into());
        self
    }

    #[must_use]
    pub const fn auto_reconnect(mut self, enable: bool) -> Self {
        self.auto_reconnect = enable;
        self
    }

    #[must_use]
    pub const fn retry(mut self, retry: Duration) -> Self {
        self.retry = retry;
        self
    }

    #[must_use]
    pub fn max_response_bytes(mut self, bytes: usize) -> Self {
        self.request = self.request.max_response_bytes(bytes);
        self
    }

    pub async fn open(self, engine: &Engine) -> Result<EventSource> {
        let EventSourceBuilder {
            mut request,
            last_event_id,
            auto_reconnect,
            retry,
        } = self;
        if let Some(id) = &last_event_id {
            request = request.header("last-event-id", id.clone())?;
        }
        let built = request.build()?;
        let url = built_url(&built);
        let headers = built_headers(&built);
        let response = engine.send(built).await?;
        Ok(EventSource {
            engine: engine.clone(),
            url,
            headers,
            body: Some(response.body),
            parser: SseParser {
                last_id: last_event_id,
                ..SseParser::default()
            },
            auto_reconnect,
            retry,
            pending: Vec::new(),
        })
    }
}

fn built_url(request: &Request) -> String {
    request.url().to_owned()
}

fn built_headers(request: &Request) -> Vec<Header> {
    request.headers().to_vec()
}

/// A live SSE response that yields [`SseEvent`] values.
pub struct EventSource {
    engine: Engine,
    url: String,
    headers: Vec<Header>,
    body: Option<ResponseBody>,
    parser: SseParser,
    auto_reconnect: bool,
    retry: Duration,
    pending: Vec<SseEvent>,
}

impl Engine {
    pub fn event_source(&self, url: impl Into<String>) -> Result<EventSourceBuilder> {
        EventSourceBuilder::new(url)
    }
}

impl EventSource {
    pub fn builder(url: impl Into<String>) -> Result<EventSourceBuilder> {
        EventSourceBuilder::new(url)
    }

    pub async fn next_event(&mut self) -> Option<Result<SseEvent>> {
        loop {
            if let Some(event) = self.pending.pop() {
                return Some(Ok(event));
            }
            let body = self.body.as_mut()?;
            match body.next_chunk().await {
                Some(Ok(chunk)) => self.ingest(&chunk),
                Some(Err(error)) => {
                    if self.auto_reconnect {
                        if let Err(error) = self.reconnect().await {
                            return Some(Err(error));
                        }
                        continue;
                    }
                    return Some(Err(error));
                }
                None => {
                    if self.auto_reconnect {
                        if let Err(error) = self.reconnect().await {
                            return Some(Err(error));
                        }
                        continue;
                    }
                    return None;
                }
            }
        }
    }

    #[must_use]
    pub fn last_event_id(&self) -> Option<&str> {
        self.parser.last_id.as_deref()
    }

    pub fn cancel(&self) {
        if let Some(body) = &self.body {
            body.cancel();
        }
    }

    #[must_use]
    pub fn handle(&self) -> Option<RequestHandle> {
        self.body.as_ref().map(ResponseBody::handle)
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.body.as_ref().is_none_or(ResponseBody::is_done)
    }

    fn ingest(&mut self, chunk: &Bytes) {
        for item in self.parser.push(chunk) {
            match item {
                ParseOutput::Event(event) => self.pending.insert(0, event),
                ParseOutput::Retry(retry) => self.retry = retry,
            }
        }
    }

    async fn reconnect(&mut self) -> Result<()> {
        if let Some(body) = self.body.take() {
            drop(body);
        }
        sleep(self.retry).await;
        let mut builder = Request::builder(&self.url)?
            .method("GET")?
            .disable_cache(true)
            .max_response_bytes(usize::MAX);
        for header in &self.headers {
            builder = builder.header(header.name(), header.value())?;
        }
        if let Some(id) = &self.parser.last_id {
            builder = builder.header("last-event-id", id.clone())?;
        }
        let response = self.engine.send(builder.build()?).await?;
        self.body = Some(response.body);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_events_comments_and_retry() {
        let mut parser = SseParser::default();
        let items = parser
            .push(b": keep-alive\nevent: update\nid: 7\nretry: 1500\ndata: hello\ndata: world\n\n");
        assert!(items.iter().any(|item| matches!(item, ParseOutput::Retry(duration) if *duration == Duration::from_millis(1500))));
        let event = items
            .into_iter()
            .find_map(|item| match item {
                ParseOutput::Event(event) => Some(event),
                ParseOutput::Retry(_) => None,
            })
            .expect("event");
        assert_eq!(event.event, "update");
        assert_eq!(event.id.as_deref(), Some("7"));
        assert_eq!(event.data, "hello\nworld");
        assert_eq!(parser.last_id.as_deref(), Some("7"));
    }

    #[test]
    fn defaults_event_type_to_message() {
        let mut parser = SseParser::default();
        let items = parser.push(b"data: ping\n\n");
        match &items[0] {
            ParseOutput::Event(event) => {
                assert_eq!(event.event, "message");
                assert_eq!(event.data, "ping");
            }
            ParseOutput::Retry(_) => panic!("expected event"),
        }
    }

    #[test]
    fn builder_accepts_loopback_url() {
        assert!(EventSourceBuilder::new("http://127.0.0.1/sse").is_ok());
        assert!(EventSource::builder("http://127.0.0.1/sse").is_ok());
    }
}
