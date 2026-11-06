use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use honk_config::node::Node;
use honk_outbound::proxy::shadowquic::ShadowQuicHandler;
use honk_outbound::proxy::{
    PacketOutbound, PacketTransport, SharedSourceOutbound, TcpOutbound, WarmRequirement,
    WarmableOutbound,
};
use honk_outbound::runtime::OutboundRuntimeRegistry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const DEADLINE: Duration = Duration::from_secs(5);

fn node(over_stream: bool) -> Node {
    let server = std::env::var("HONK_SHADOWQUIC_TEST_SERVER")
        .expect("run through spike/run-handler-tests.sh");
    Node::from_share_link(&format!("shadowquic://spike-user:spike-pass@{server}?sni=shadowquic.test&over-stream={over_stream}#test")).unwrap()
}

struct Server {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn tcp_echo(ip: &str) -> Server {
    let listener = TcpListener::bind(format!("{ip}:0")).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut streams = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let Ok((mut stream, _)) = result else { break };
                    streams.spawn(async move {
                        let (mut read, mut write) = stream.split();
                        let _ = tokio::io::copy(&mut read, &mut write).await;
                        let _ = write.shutdown().await;
                    });
                }
                Some(_) = streams.join_next(), if !streams.is_empty() => {}
            }
        }
    });
    Server { address, task }
}

async fn udp_echo(ip: &str) -> Server {
    let socket = UdpSocket::bind(format!("{ip}:0")).await.unwrap();
    let address = socket.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut buffer = vec![0; 65535];
        while let Ok((length, peer)) = socket.recv_from(&mut buffer).await {
            if socket.send_to(&buffer[..length], peer).await.is_err() {
                break;
            }
        }
    });
    Server { address, task }
}

async fn packet_echo(transport: &dyn PacketTransport, payload: &[u8], peer: SocketAddr) {
    transport.send_packet(payload).await.unwrap();
    let mut buffer = vec![0; 65535];
    let (length, source) = tokio::time::timeout(DEADLINE, transport.recv_packet(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buffer[..length], payload);
    assert_eq!(source, peer);
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn authenticated_tcp_concurrency_domain_and_ipv6() {
    let node = node(false);
    let registry = OutboundRuntimeRegistry::build(std::slice::from_ref(&node)).unwrap();
    let runtime = registry.get(&node.id).unwrap();
    let echo = tcp_echo("127.0.0.1").await;
    let mut jobs = tokio::task::JoinSet::new();
    for index in 0..8 {
        let runtime = Arc::clone(&runtime);
        let target = echo.address;
        jobs.spawn(async move {
            let handler = ShadowQuicHandler::new();
            let domain = (index % 2 == 0).then_some("localhost");
            let mut stream = handler
                .dial_runtime(runtime, target, domain, DEADLINE)
                .await
                .unwrap();
            let payload = format!("stream-{index}").into_bytes();
            stream.stream.write_all(&payload).await.unwrap();
            let mut response = vec![0; payload.len()];
            stream.stream.read_exact(&mut response).await.unwrap();
            assert_eq!(response, payload);
        });
    }
    while let Some(job) = jobs.join_next().await {
        job.unwrap();
    }
    let v6 = tcp_echo("[::1]").await;
    let mut stream = ShadowQuicHandler::new()
        .dial_runtime(runtime, v6.address, None, DEADLINE)
        .await
        .unwrap();
    stream.stream.write_all(b"ipv6").await.unwrap();
    let mut response = [0; 4];
    tokio::time::timeout(DEADLINE, stream.stream.read_exact(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&response, b"ipv6");
    registry.shutdown().await;
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn tcp_half_close_preserves_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        socket.read_to_end(&mut request).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        socket.write_all(&request).await.unwrap();
    });
    let handler = ShadowQuicHandler::new();
    let mut stream = handler
        .dial(&node(false), target, None, DEADLINE)
        .await
        .unwrap();
    stream.stream.write_all(b"after-fin").await.unwrap();
    stream.stream.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(DEADLINE, stream.stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response, b"after-fin");
    server.await.unwrap();
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn bad_credentials_never_open_the_tcp_target() {
    for change_user in [false, true] {
        let mut node = node(false);
        let config = node.shadowquic_mut().unwrap();
        if change_user {
            config.username = Some("wrong-user".into());
        } else {
            config.password = Some("wrong-password".into());
        }
        node.id = node.derive_id();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let result = ShadowQuicHandler::new()
            .dial(
                &node,
                listener.local_addr().unwrap(),
                None,
                Duration::from_millis(500),
            )
            .await;
        assert!(result.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn udp_modes_independent_associations_sources_and_size_limits() {
    let echo = udp_echo("127.0.0.1").await;
    let handler = ShadowQuicHandler::new();
    for over_stream in [false, true] {
        let node = node(over_stream);
        let first = handler
            .dial_udp_transport(&node, echo.address, Some("localhost"), DEADLINE)
            .await
            .unwrap();
        let second = handler
            .dial_udp_transport(&node, echo.address, None, DEADLINE)
            .await
            .unwrap();
        first.send_packet(b"first-association").await.unwrap();
        second.send_packet(b"second-association").await.unwrap();
        let mut buffer = [0; 128];
        let (length, peer) = tokio::time::timeout(DEADLINE, first.recv_packet(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..length], b"first-association");
        assert_eq!(peer, echo.address);
        let (length, peer) = tokio::time::timeout(DEADLINE, second.recv_packet(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..length], b"second-association");
        assert_eq!(peer, echo.address);
        let oversized = vec![0; 65508];
        let error = first.send_packet(&oversized).await.unwrap_err();
        assert!(honk_outbound::proxy::is_packet_rejection(
            &anyhow::Error::new(error)
        ));
        packet_echo(first.as_ref(), b"still-usable", echo.address).await;
        packet_echo(first.as_ref(), b"", echo.address).await;
        let large = vec![7; 60000];
        if over_stream {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let target = socket.local_addr().unwrap();
            let received = tokio::spawn(async move {
                let mut buffer = vec![0; 65535];
                let (length, peer) = socket.recv_from(&mut buffer).await.unwrap();
                assert_eq!(length, 60000);
                assert!(buffer[..length].iter().all(|byte| *byte == 7));
                socket.send_to(b"large-ok", peer).await.unwrap();
            });
            let transport = handler.dial_udp_transport(&node, target, None, DEADLINE).await.unwrap();
            transport.send_packet(&large).await.unwrap();
            let (length, peer) = tokio::time::timeout(DEADLINE, transport.recv_packet(&mut buffer)).await.unwrap().unwrap();
            assert_eq!(&buffer[..length], b"large-ok");
            assert_eq!(peer, target);
            received.await.unwrap();
        } else {
            assert!(first.send_packet(&large).await.is_err());
        }
        let ipv6 = udp_echo("[::1]").await;
        let v6 = handler
            .dial_udp_transport(&node, ipv6.address, None, DEADLINE)
            .await
            .unwrap();
        packet_echo(v6.as_ref(), b"udp-ipv6", ipv6.address).await;

        let request_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let reply_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = request_socket.local_addr().unwrap();
        let real_peer = reply_socket.local_addr().unwrap();
        let replies = tokio::spawn(async move {
            let mut data = [0; 128];
            let (length, client) = request_socket.recv_from(&mut data).await.unwrap();
            reply_socket.send_to(&data[..length], client).await.unwrap();
        });
        let transport = handler
            .dial_udp_transport(&node, target, None, DEADLINE)
            .await
            .unwrap();
        packet_echo(transport.as_ref(), b"actual-source", real_peer).await;
        replies.await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn speculative_udp_warm_release_and_generation_shutdown() {
    let echo = udp_echo("127.0.0.1").await;
    let other = udp_echo("127.0.0.1").await;
    let handler = ShadowQuicHandler::new();
    for over_stream in [false, true] {
        let node = node(over_stream);
        let registry = OutboundRuntimeRegistry::build(std::slice::from_ref(&node)).unwrap();
        let runtime = registry.get(&node.id).unwrap();
        let (left, right) = tokio::join!(
            handler.prepare_shared_source(Arc::clone(&runtime), echo.address, Some("localhost"), DEADLINE, true),
            handler.prepare_shared_source(
                Arc::clone(&runtime),
                other.address,
                None,
                DEADLINE,
                true
            ),
        );
        assert_eq!(runtime.warm_counts().clients, Some(0));
        handler
            .warm(Arc::clone(&runtime), DEADLINE, WarmRequirement::Session)
            .await
            .unwrap();
        let (left, right) = tokio::join!(left.unwrap().commit(), right.unwrap().commit());
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(runtime.warm_counts().clients, Some(1));
        packet_echo(left.as_ref(), b"left", echo.address).await;
        packet_echo(right.as_ref(), b"right", other.address).await;
        left.send_to(other.address, Some("localhost"), b"second-target", true, None)
            .await
            .unwrap();
        let mut buffer = [0; 64];
        let (length, peer) = tokio::time::timeout(DEADLINE, left.recv_packet(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..length], b"second-target");
        assert_eq!(peer, other.address);
        left.send_to(echo.address, Some("unresolved.invalid"), b"other-name", true, None).await.unwrap();
        let (length, peer) = tokio::time::timeout(DEADLINE, left.recv_packet(&mut buffer)).await.unwrap().unwrap();
        assert_eq!(&buffer[..length], b"other-name");
        assert_eq!(peer, echo.address);
        let echo_tcp = tcp_echo("127.0.0.1").await;
        let mut active_tcp = handler
            .dial_runtime(Arc::clone(&runtime), echo_tcp.address, None, DEADLINE)
            .await
            .unwrap();
        registry.retire_reusable_state().await;
        assert_eq!(runtime.warm_counts().clients, Some(0));
        packet_echo(left.as_ref(), b"after-warm-release", echo.address).await;
        active_tcp
            .stream
            .write_all(b"tcp-after-release")
            .await
            .unwrap();
        let mut tcp_reply = [0; 17];
        tokio::time::timeout(DEADLINE, active_tcp.stream.read_exact(&mut tcp_reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&tcp_reply, b"tcp-after-release");
        tokio::time::timeout(DEADLINE, registry.shutdown())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(DEADLINE, left.recv_packet(&mut buffer))
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            tokio::time::timeout(DEADLINE, right.recv_packet(&mut buffer))
                .await
                .unwrap()
                .is_err()
        );
    }
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn abandoned_speculation_and_cancelled_handshake_are_joined() {
    let echo = udp_echo("127.0.0.1").await;
    let handler = ShadowQuicHandler::new();
    let node = node(false);
    let registry = OutboundRuntimeRegistry::build(std::slice::from_ref(&node)).unwrap();
    let runtime = registry.get(&node.id).unwrap();
    let pending = handler
        .prepare_shared_source(Arc::clone(&runtime), echo.address, None, DEADLINE, true)
        .await
        .unwrap();
    drop(pending);
    assert_eq!(runtime.warm_counts().clients, Some(0));
    let mut bad = node.clone();
    bad.shadowquic_mut().unwrap().password = Some("wrong".into());
    bad.id = bad.derive_id();
    let bad_registry = OutboundRuntimeRegistry::build(std::slice::from_ref(&bad)).unwrap();
    let bad_runtime = bad_registry.get(&bad.id).unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            handler.prepare_shared_source(bad_runtime, echo.address, None, DEADLINE, true)
        )
        .await
        .is_err()
    );
    tokio::time::timeout(DEADLINE, bad_registry.shutdown())
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, registry.shutdown())
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn context_exhaustion_is_local_and_existing_destinations_survive() {
    let handler = ShadowQuicHandler::new();
    for over_stream in [false, true] {
        let limit = if over_stream { 16 } else { 256 };
        let mut servers = Vec::new();
        for _ in 0..=limit {
            servers.push(udp_echo("127.0.0.1").await);
        }
        let node = node(over_stream);
        let registry = OutboundRuntimeRegistry::build(std::slice::from_ref(&node)).unwrap();
        let source = handler
            .prepare_shared_source(
                registry.get(&node.id).unwrap(),
                servers[0].address,
                None,
                DEADLINE,
                false,
            )
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        let mut buffer = [0; 32];
        for (index, server) in servers[..limit].iter().enumerate() {
            let bytes = (index as u16).to_be_bytes();
            source
                .send_to(server.address, None, &bytes, true, None)
                .await
                .unwrap();
            let (length, peer) = tokio::time::timeout(DEADLINE, source.recv_packet(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(peer, server.address);
            assert_eq!(&buffer[..length], &bytes);
        }
        let error = source
            .send_to(servers[limit].address, None, b"over-limit", true, None)
            .await
            .unwrap_err();
        assert!(honk_outbound::proxy::is_packet_rejection(
            &anyhow::Error::new(error)
        ));
        assert!(!source.frame_stream_torn());
        packet_echo(source.as_ref(), b"existing-context", servers[0].address).await;
        registry.shutdown().await;
    }
}

#[tokio::test]
#[ignore = "requires the pinned reference server"]
async fn endpoint_capacity_is_bounded_and_released_after_drop() {
    let handler = ShadowQuicHandler::new();
    let node = node(false);
    let registry = OutboundRuntimeRegistry::build(std::slice::from_ref(&node)).unwrap();
    let runtime = registry.get(&node.id).unwrap();
    let target = "127.0.0.1:12345".parse().unwrap();
    let mut associations = Vec::new();
    for _ in 0..64 {
        associations.push(
            handler
                .dial_udp_transport_runtime(Arc::clone(&runtime), target, None, DEADLINE)
                .await
                .unwrap(),
        );
    }
    let error = handler
        .dial_udp_transport_runtime(Arc::clone(&runtime), target, None, DEADLINE)
        .await
        .unwrap_err();
    assert!(honk_outbound::proxy::is_packet_rejection(&error));
    associations.pop();
    let replacement = handler
        .dial_udp_transport_runtime(runtime, target, None, DEADLINE)
        .await
        .unwrap();
    drop(replacement);
    tokio::time::timeout(DEADLINE, registry.shutdown())
        .await
        .unwrap();
}
