#![cfg(feature = "dns")]

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use cronet::dns::{
    DnsError, DnsResolver, LookupIpStrategy, RData, RecordType, ResolveHosts, ResolverConfig,
    ResolverOpts,
};
use hickory_resolver::proto::{
    op::{Message, MessageType, ResponseCode},
    rr::{
        Name, Record,
        rdata::{A, PTR, TXT},
    },
};
use tokio::{net::UdpSocket, sync::oneshot, task::JoinHandle, time::timeout};

const TEST_ADDRESS: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);

struct TestDnsServer {
    address: SocketAddr,
    queries: Arc<AtomicUsize>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl TestDnsServer {
    async fn start() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let queries = Arc::new(AtomicUsize::new(0));
        let task_queries = Arc::clone(&queries);
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut buffer = [0_u8; 4096];
            loop {
                tokio::select! {
                    result = socket.recv_from(&mut buffer) => {
                        let (length, peer) = result.unwrap();
                        task_queries.fetch_add(1, Ordering::SeqCst);
                        let request = Message::from_vec(&buffer[..length]).unwrap();
                        let response = response_for(&request);
                        let response = response.to_vec().unwrap();
                        socket.send_to(&response, peer).await.unwrap();
                    }
                    _ = &mut shutdown_rx => break,
                }
            }
        });

        Self {
            address,
            queries,
            shutdown: Some(shutdown),
            task,
        }
    }

    fn query_count(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }

    async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.await.unwrap();
    }
}

fn response_for(request: &Message) -> Message {
    let query = request.queries().first().unwrap();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_op_code(request.op_code())
        .set_authoritative(true)
        .set_recursion_desired(request.recursion_desired())
        .set_recursion_available(true)
        .add_query(query.clone());

    if query.name().to_ascii() == "missing.test." {
        response.set_response_code(ResponseCode::NXDomain);
        return response;
    }

    let data = match query.query_type() {
        RecordType::A => RData::A(A(TEST_ADDRESS)),
        RecordType::TXT => RData::TXT(TXT::new(vec!["cronet-rs-dns-e2e".to_owned()])),
        RecordType::PTR => RData::PTR(PTR(
            Name::from_str("service.test.").expect("valid test name")
        )),
        record_type => panic!("unexpected DNS query type: {record_type}"),
    };
    response.add_answer(Record::from_rdata(query.name().clone(), 60, data));
    response
}

fn test_options() -> ResolverOpts {
    let mut options = ResolverOpts::default();
    options.attempts = 1;
    options.timeout = Duration::from_millis(500);
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    options.use_hosts_file = ResolveHosts::Never;
    options
}

#[tokio::test]
async fn dns_resolver_e2e_covers_queries_cache_configuration_and_errors() {
    timeout(Duration::from_secs(10), async {
        let server = TestDnsServer::start().await;
        let resolver = DnsResolver::from_name_servers([server.address], test_options()).unwrap();

        assert_eq!(resolver.config().name_servers().len(), 2);
        assert_eq!(resolver.options().attempts, 1);
        assert!(format!("{resolver:?}").contains("DnsResolver"));

        let addresses = resolver
            .lookup_ip("service.test.")
            .await
            .unwrap()
            .iter()
            .collect::<Vec<_>>();
        assert_eq!(addresses, [IpAddr::V4(TEST_ADDRESS)]);

        let txt = resolver
            .lookup("service.test.", RecordType::TXT)
            .await
            .unwrap();
        assert!(txt.iter().any(|record| {
            matches!(
                record,
                RData::TXT(value)
                    if value.iter().any(|part| part.as_ref() == b"cronet-rs-dns-e2e")
            )
        }));

        resolver
            .lookup("service.test.", RecordType::TXT)
            .await
            .unwrap();
        assert_eq!(
            server.query_count(),
            2,
            "the second TXT lookup must hit cache"
        );

        let reverse = resolver
            .reverse_lookup(IpAddr::V4(TEST_ADDRESS))
            .await
            .unwrap();
        assert_eq!(reverse.iter().next().unwrap().0.to_ascii(), "service.test.");

        let error = resolver
            .lookup("missing.test.", RecordType::A)
            .await
            .unwrap_err();
        assert!(matches!(error, DnsError::Resolve(_)));
        assert!(error.is_nx_domain());
        assert!(error.is_no_records_found());
        assert!(error.resolve_error().is_some());
        assert!(std::error::Error::source(&error).is_some());
        assert!(error.to_string().starts_with("DNS resolution failed:"));

        resolver.clear_cache();
        resolver
            .lookup("service.test.", RecordType::TXT)
            .await
            .unwrap();
        assert_eq!(
            server.query_count(),
            5,
            "cache clear must force a new query"
        );

        let explicit = DnsResolver::from_config(
            ResolverConfig::from_parts(None, Vec::new(), resolver.config().name_servers().to_vec()),
            test_options(),
        );
        assert_eq!(
            explicit
                .lookup_ip("service.test.")
                .await
                .unwrap()
                .iter()
                .next(),
            Some(IpAddr::V4(TEST_ADDRESS))
        );

        server.shutdown().await;
    })
    .await
    .expect("local DNS E2E timed out");
}

#[test]
fn system_dns_configuration_can_be_loaded() {
    let resolver = DnsResolver::from_system().expect("host DNS configuration should be readable");
    assert!(!resolver.config().name_servers().is_empty());
}
