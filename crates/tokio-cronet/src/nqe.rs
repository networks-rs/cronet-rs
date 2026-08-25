//! Network quality estimates derived from Cronet request-finished metrics.

use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{runtime::Handle, sync::watch};

use crate::{Engine, EngineBuilder, Error, RequestFinishedInfo, Result};

const HTTP_RTT_SLOW_2G_MS: i64 = 2010;
const HTTP_RTT_2G_MS: i64 = 1400;
const HTTP_RTT_3G_MS: i64 = 270;
const SAMPLE_LIMIT: usize = 32;

/// Chromium-compatible effective connection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectiveConnectionType {
    Unknown,
    Offline,
    Slow2G,
    TwoG,
    ThreeG,
    FourG,
}

/// Latest HTTP RTT, transport RTT, and downstream throughput.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkQualityEstimates {
    pub http_rtt: Option<Duration>,
    pub transport_rtt: Option<Duration>,
    pub downstream_throughput_kbps: Option<i32>,
    pub effective_connection_type: EffectiveConnectionType,
}

impl Default for NetworkQualityEstimates {
    fn default() -> Self {
        Self {
            http_rtt: None,
            transport_rtt: None,
            downstream_throughput_kbps: None,
            effective_connection_type: EffectiveConnectionType::Unknown,
        }
    }
}

pub(crate) struct NqeState {
    skip_localhost: AtomicBool,
    samples: Mutex<VecDeque<Sample>>,
    latest: Mutex<NetworkQualityEstimates>,
    updates: watch::Sender<NetworkQualityEstimates>,
}

#[derive(Clone, Copy)]
struct Sample {
    http_rtt_ms: Option<i64>,
    transport_rtt_ms: Option<i64>,
    throughput_kbps: Option<i32>,
}

impl NqeState {
    pub(crate) fn start(
        mut finished: tokio::sync::broadcast::Receiver<RequestFinishedInfo>,
        handle: &Handle,
    ) -> std::sync::Arc<Self> {
        let (updates, _) = watch::channel(NetworkQualityEstimates::default());
        let state = std::sync::Arc::new(Self {
            skip_localhost: AtomicBool::new(true),
            samples: Mutex::new(VecDeque::new()),
            latest: Mutex::new(NetworkQualityEstimates::default()),
            updates,
        });
        let task = state.clone();
        handle.spawn(async move {
            loop {
                match finished.recv().await {
                    Ok(info) => task.observe(&info),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        state
    }

    pub(crate) fn configure_for_testing(&self, use_localhost: bool) {
        self.skip_localhost.store(!use_localhost, Ordering::Release);
    }

    pub(crate) fn estimates(&self) -> NetworkQualityEstimates {
        *self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<NetworkQualityEstimates> {
        self.updates.subscribe()
    }

    fn observe(&self, info: &RequestFinishedInfo) {
        if self.skip_localhost.load(Ordering::Acquire) && is_localhost(info) {
            return;
        }
        let sample = Sample {
            http_rtt_ms: http_rtt_ms(&info.metrics),
            transport_rtt_ms: transport_rtt_ms(&info.metrics),
            throughput_kbps: throughput_kbps(&info.metrics),
        };
        let estimates = {
            let mut samples = self
                .samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            samples.push_back(sample);
            while samples.len() > SAMPLE_LIMIT {
                samples.pop_front();
            }
            summarize(&samples)
        };
        *self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = estimates;
        let _ = self.updates.send(estimates);
    }
}

fn is_localhost(info: &RequestFinishedInfo) -> bool {
    let Some(url) = info.response.as_ref().map(|response| response.url.as_str()) else {
        return false;
    };
    url.contains("://127.0.0.1") || url.contains("://localhost") || url.contains("://[::1]")
}

fn http_rtt_ms(metrics: &crate::RequestMetrics) -> Option<i64> {
    match (metrics.response_start, metrics.sending_end) {
        (Some(response_start), Some(sending_end)) if response_start >= sending_end => {
            Some(response_start - sending_end)
        }
        _ => match (metrics.request_end, metrics.request_start) {
            (Some(end), Some(start)) if end >= start => Some(end - start),
            _ => None,
        },
    }
}

fn transport_rtt_ms(metrics: &crate::RequestMetrics) -> Option<i64> {
    match (metrics.connect_end, metrics.connect_start) {
        (Some(end), Some(start)) if end >= start => Some(end - start),
        _ => None,
    }
}

fn throughput_kbps(metrics: &crate::RequestMetrics) -> Option<i32> {
    let duration = match (metrics.request_end, metrics.request_start) {
        (Some(end), Some(start)) if end > start => end - start,
        _ => return None,
    };
    if duration == 0 || metrics.received_byte_count <= 0 {
        return None;
    }
    let kbps = metrics.received_byte_count.saturating_mul(8) / duration;
    i32::try_from(kbps).ok()
}

fn summarize(samples: &VecDeque<Sample>) -> NetworkQualityEstimates {
    let http = median(samples.iter().filter_map(|sample| sample.http_rtt_ms));
    let transport = median(samples.iter().filter_map(|sample| sample.transport_rtt_ms));
    let throughput = median(
        samples
            .iter()
            .filter_map(|sample| sample.throughput_kbps.map(i64::from)),
    );
    let ect = effective_connection_type(http);
    NetworkQualityEstimates {
        http_rtt: http.and_then(|ms| u64::try_from(ms).ok().map(Duration::from_millis)),
        transport_rtt: transport.and_then(|ms| u64::try_from(ms).ok().map(Duration::from_millis)),
        downstream_throughput_kbps: throughput.and_then(|kbps| i32::try_from(kbps).ok()),
        effective_connection_type: ect,
    }
}

fn median(values: impl Iterator<Item = i64>) -> Option<i64> {
    let mut values = values.collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn effective_connection_type(http_rtt_ms: Option<i64>) -> EffectiveConnectionType {
    match http_rtt_ms {
        None => EffectiveConnectionType::Unknown,
        Some(ms) if ms >= HTTP_RTT_SLOW_2G_MS => EffectiveConnectionType::Slow2G,
        Some(ms) if ms >= HTTP_RTT_2G_MS => EffectiveConnectionType::TwoG,
        Some(ms) if ms >= HTTP_RTT_3G_MS => EffectiveConnectionType::ThreeG,
        Some(_) => EffectiveConnectionType::FourG,
    }
}

impl Engine {
    /// Includes loopback traffic in estimates. Chromium NQE ignores localhost
    /// unless this testing override is set.
    pub fn configure_network_quality_estimator_for_testing(
        &self,
        use_localhost_requests: bool,
        _use_smaller_responses: bool,
        _disable_offline_check: bool,
    ) -> Result<()> {
        let nqe = self.nqe()?;
        nqe.configure_for_testing(use_localhost_requests);
        Ok(())
    }

    pub fn effective_connection_type(&self) -> Result<EffectiveConnectionType> {
        Ok(self.nqe()?.estimates().effective_connection_type)
    }

    pub fn http_rtt(&self) -> Result<Option<Duration>> {
        Ok(self.nqe()?.estimates().http_rtt)
    }

    pub fn transport_rtt(&self) -> Result<Option<Duration>> {
        Ok(self.nqe()?.estimates().transport_rtt)
    }

    pub fn downstream_throughput_kbps(&self) -> Result<Option<i32>> {
        Ok(self.nqe()?.estimates().downstream_throughput_kbps)
    }

    pub fn network_quality(&self) -> Result<NetworkQualityEstimates> {
        Ok(self.nqe()?.estimates())
    }

    pub fn subscribe_network_quality(&self) -> Result<watch::Receiver<NetworkQualityEstimates>> {
        Ok(self.nqe()?.subscribe())
    }

    fn nqe(&self) -> Result<&NqeState> {
        self.inner
            .nqe
            .as_deref()
            .ok_or(Error::NetworkQualityEstimatorDisabled)
    }
}

impl EngineBuilder {
    #[must_use]
    pub const fn enable_network_quality_estimator(mut self, enable: bool) -> Self {
        self.enable_network_quality_estimator = enable;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RequestMetrics;

    fn metrics(http: i64, received: i64) -> RequestMetrics {
        RequestMetrics {
            request_start: Some(0),
            dns_start: None,
            dns_end: None,
            connect_start: Some(0),
            connect_end: Some(20),
            ssl_start: None,
            ssl_end: None,
            sending_start: Some(20),
            sending_end: Some(30),
            push_start: None,
            push_end: None,
            response_start: Some(30 + http),
            request_end: Some(40 + http),
            socket_reused: false,
            sent_byte_count: 100,
            received_byte_count: received,
        }
    }

    #[test]
    fn classifies_http_rtt_thresholds() {
        assert_eq!(
            effective_connection_type(Some(3000)),
            EffectiveConnectionType::Slow2G
        );
        assert_eq!(
            effective_connection_type(Some(1500)),
            EffectiveConnectionType::TwoG
        );
        assert_eq!(
            effective_connection_type(Some(400)),
            EffectiveConnectionType::ThreeG
        );
        assert_eq!(
            effective_connection_type(Some(50)),
            EffectiveConnectionType::FourG
        );
        assert_eq!(
            effective_connection_type(None),
            EffectiveConnectionType::Unknown
        );
    }

    #[test]
    fn reads_http_rtt_from_response_gap() {
        assert_eq!(http_rtt_ms(&metrics(80, 1000)), Some(80));
        assert_eq!(transport_rtt_ms(&metrics(80, 1000)), Some(20));
        assert!(throughput_kbps(&metrics(80, 10_000)).is_some());
    }
}
