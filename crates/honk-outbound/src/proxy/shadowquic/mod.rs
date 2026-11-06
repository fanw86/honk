//! JLS-authenticated ShadowQUIC client. UDP associations isolate connection-wide IDs.

mod client;
mod tcp;
mod udp;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use honk_config::node::Node;

use super::{MultiTargetTransport, PacketOutbound, PacketTransport, PreparedUdpTransport,
    ProbeableOutbound, ProxyStream, SharedSourceOutbound, TcpOutbound, WarmableOutbound, WarmRequirement};
use client::Client;

#[derive(Debug, Default, Clone)]
pub struct ShadowQuicHandler;

impl ShadowQuicHandler {
    pub fn new() -> Self { Self }

    async fn client(&self, runtime: &crate::runtime::NodeRuntime) -> Result<Arc<Client>> {
        runtime.quic_client(|| async { Client::new(runtime.node.as_ref()) }).await
    }

    async fn tcp(&self, runtime: Arc<crate::runtime::NodeRuntime>, target: SocketAddr, domain: Option<&str>, timeout: Duration) -> Result<ProxyStream> {
        let operation = async {
            let address = super::addr::SocksAddr::new(target, domain)?;
            let client = self.client(&runtime).await?;
            let lease = client.tcp(timeout).await?;
            crate::runtime::start_scoped_dial();
            let observation = crate::session::ObservedSessionOpen::start();
            let opened = async {
                let (mut send, recv) = lease.0.conn.open_bi().await?;
                let mut header = vec![0x01];
                address.encode(&mut header);
                send.write_all(&header).await?;
                Ok::<_, anyhow::Error>((send, recv))
            }.await;
            let (send, recv) = match opened {
                Ok(stream) => {
                    observation.finish(crate::runtime::flow_observation::SessionEvent::OpenSucceeded);
                    stream
                }
                Err(error) => {
                    observation.finish(crate::runtime::flow_observation::SessionEvent::OpenFailed);
                    return Err(error);
                }
            };
            publish_request_sent();
            Ok(ProxyStream {
                stream: Box::new(tcp::Stream::new(send, recv, lease)),
                target_addr: target, target_domain: domain.map(str::to_string),
            })
        };
        runtime.scope_tasks(async { tokio::time::timeout(timeout, operation).await.context("ShadowQUIC TCP dial deadline")? }).await
    }

    async fn packet(&self, runtime: Arc<crate::runtime::NodeRuntime>, target: SocketAddr, domain: Option<&str>, timeout: Duration) -> Result<Arc<udp::Udp>> {
        let operation = async {
            super::addr::SocksAddr::new(target, domain)?;
            let client = self.client(&runtime).await?;
            let session = client.connect(timeout).await?;
            udp::Udp::new(session, target, domain, runtime.node.shadowquic().expect("ShadowQUIC runtime").over_stream).await
        };
        runtime.scope_tasks(async { tokio::time::timeout(timeout, operation).await.context("ShadowQUIC UDP dial deadline")? }).await
    }

    async fn prepare_source(&self, runtime: Arc<crate::runtime::NodeRuntime>, target: SocketAddr, domain: Option<&str>, timeout: Duration, speculative: bool) -> Result<PreparedUdpTransport<dyn MultiTargetTransport>> {
        super::addr::SocksAddr::new(target, domain)?;
        if !speculative {
            return Ok(PreparedUdpTransport::ready(self.packet(runtime, target, domain, timeout).await? as Arc<dyn MultiTargetTransport>));
        }
        let client = Client::new(runtime.node.as_ref())?;
        let over_stream = runtime.node.shadowquic().expect("ShadowQUIC runtime").over_stream;
        super::prepare_detached_quic_transport(runtime, client, |client| async move {
            let operation = async {
                let session = client.connect(timeout).await?;
                Ok(udp::Udp::new(session, target, domain, over_stream).await? as Arc<dyn MultiTargetTransport>)
            };
            tokio::time::timeout(timeout, operation).await.context("ShadowQUIC speculative dial deadline")?
        }).await
    }
}

pub(super) fn publish_request_sent() {
    if let Some(observer) = crate::runtime::flow_observation::current() {
        observer.milestone_once(crate::runtime::flow_observation::Milestone::TargetRequestSent);
    }
}

#[async_trait]
impl TcpOutbound for ShadowQuicHandler {
    async fn dial(&self, node: &Node, target: SocketAddr, domain: Option<&str>, timeout: Duration) -> Result<ProxyStream> {
        let guard = crate::runtime::NodeRuntime::try_ephemeral_guarded(node)?;
        Ok(self.tcp(guard.runtime(), target, domain, timeout).await?.with_owner(guard))
    }
    async fn dial_runtime(&self, runtime: Arc<crate::runtime::NodeRuntime>, target: SocketAddr, domain: Option<&str>, timeout: Duration) -> Result<ProxyStream> {
        self.tcp(runtime, target, domain, timeout).await
    }
    async fn dial_with_tcp(&self, _node: &Node, _target: SocketAddr, _domain: Option<&str>, _tcp: tokio::net::TcpStream, _timeout: Duration) -> Result<ProxyStream> {
        anyhow::bail!("ShadowQUIC requires QUIC and cannot reuse a TCP connection")
    }
}

#[async_trait]
impl PacketOutbound for ShadowQuicHandler {
    async fn dial_udp_transport(&self, node: &Node, target: SocketAddr, domain: Option<&str>, timeout: Duration) -> Result<Arc<dyn PacketTransport>> {
        let guard = crate::runtime::NodeRuntime::try_ephemeral_guarded(node)?;
        let transport = self.packet(guard.runtime(), target, domain, timeout).await?;
        Ok(super::packet_transport_with_owner(transport, guard))
    }
    async fn dial_udp_transport_runtime(&self, runtime: Arc<crate::runtime::NodeRuntime>, target: SocketAddr, domain: Option<&str>, timeout: Duration) -> Result<Arc<dyn PacketTransport>> {
        Ok(self.packet(runtime, target, domain, timeout).await?)
    }
    async fn dial_udp_transport_speculative_runtime(&self, runtime: Arc<crate::runtime::NodeRuntime>, target: SocketAddr, domain: Option<&str>, timeout: Duration) -> Result<PreparedUdpTransport> {
        let prepared = self.prepare_source(runtime, target, domain, timeout, true).await?;
        Ok(PreparedUdpTransport::new(async move { Ok(prepared.commit().await? as Arc<dyn PacketTransport>) }))
    }
}

#[async_trait]
impl SharedSourceOutbound for ShadowQuicHandler {
    async fn prepare_shared_source(&self, runtime: Arc<crate::runtime::NodeRuntime>, target: SocketAddr, domain: Option<&str>, timeout: Duration, speculative: bool) -> Result<PreparedUdpTransport<dyn MultiTargetTransport>> {
        self.prepare_source(runtime, target, domain, timeout, speculative).await
    }
}

#[async_trait]
impl WarmableOutbound for ShadowQuicHandler {
    async fn warm(&self, runtime: Arc<crate::runtime::NodeRuntime>, timeout: Duration, requirement: WarmRequirement) -> Result<()> {
        let operation = async {
            let client = self.client(&runtime).await?;
            let lease = client.tcp(timeout).await?;
            if matches!(requirement, WarmRequirement::Udp) && !runtime.node.shadowquic().expect("ShadowQUIC runtime").over_stream && lease.0.conn.max_datagram_size().is_none() {
                anyhow::bail!("ShadowQUIC peer did not negotiate datagrams");
            }
            Ok(())
        };
        runtime.scope_tasks(crate::runtime::flow_observation::without(async {
            tokio::time::timeout(timeout, operation).await.context("ShadowQUIC warm deadline")?
        })).await
    }
}

#[async_trait]
impl ProbeableOutbound for ShadowQuicHandler {
    async fn test_connectivity(&self, node: &Node) -> bool {
        let Ok(mut guard) = crate::runtime::NodeRuntime::try_ephemeral_guarded(node) else { return false };
        let result = self.warm(guard.runtime(), Duration::from_secs(5), WarmRequirement::Session).await.is_ok();
        let _ = guard.close().await;
        result
    }
}
