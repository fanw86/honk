use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use honk_config::node::{Node, ShadowQuicConfig};
use parking_lot::Mutex;
use quinn_jls as quinn;

const MAX_ENDPOINTS: usize = 64;
const MAX_WORKERS: usize = 64;
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) struct Owner {
    pub endpoint: crate::runtime::RuntimeEndpoint<quinn::Endpoint>,
    workers: Mutex<Workers>,
}

#[derive(Default)]
struct Workers {
    closed: bool,
    tasks: Vec<crate::runtime::SharedTask>,
}

impl Owner {
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
        let mut workers = self.workers.lock();
        if workers.closed {
            return Err(super::super::PacketRejection::Cancelled.into());
        }
        workers.tasks.retain(|task| !task.is_finished());
        if workers.tasks.len() >= MAX_WORKERS {
            return Err(super::super::PacketRejection::Capacity.into());
        }
        workers
            .tasks
            .push(crate::runtime::spawn_joinable(None, future)?);
        Ok(())
    }

    pub fn abort(&self) {
        let mut workers = self.workers.lock();
        workers.closed = true;
        self.endpoint
            .close(0u32.into(), b"ShadowQUIC association closed");
        for task in &workers.tasks {
            task.abort();
        }
    }

    async fn close(&self) {
        self.abort();
        let tasks = self.workers.lock().tasks.clone();
        for task in tasks {
            let _ = task.join().await;
        }
        self.endpoint.wait_idle().await;
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.abort();
    }
}

pub(super) struct Session {
    pub conn: quinn::Connection,
    pub owner: Arc<Owner>,
    leases: Mutex<Leases>,
}

struct Leases {
    active: usize,
    last_used: tokio::time::Instant,
    retired: bool,
}

impl Session {
    fn lease(self: &Arc<Self>) -> Option<Lease> {
        let mut leases = self.leases.lock();
        if leases.retired || self.conn.close_reason().is_some() {
            return None;
        }
        leases.active += 1;
        Some(Lease(Arc::clone(self)))
    }

    fn reap_idle(&self) -> bool {
        let mut leases = self.leases.lock();
        if self.conn.close_reason().is_some() {
            return true;
        }
        if leases.active == 0 && leases.last_used.elapsed() >= IDLE_TIMEOUT {
            leases.retired = true;
            self.conn.close(0u32.into(), b"ShadowQUIC idle");
            return true;
        }
        false
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.owner.abort();
    }
}

pub(super) struct Lease(pub Arc<Session>);

impl Drop for Lease {
    fn drop(&mut self) {
        let mut leases = self.0.leases.lock();
        leases.active -= 1;
        leases.last_used = tokio::time::Instant::now();
    }
}

#[derive(Default)]
struct Owners {
    closed: bool,
    endpoints: Vec<Weak<Owner>>,
}

pub(super) struct Client {
    host: String,
    port: u16,
    name: String,
    crypto: quinn::ClientConfig,
    owners: Mutex<Owners>,
    cached: tokio::sync::Mutex<Option<Arc<Session>>>,
}

impl Client {
    pub fn new(node: &Node) -> Result<Arc<Self>> {
        node.validate()?;
        let config = node
            .shadowquic()
            .ok_or_else(|| anyhow!("expected ShadowQUIC configuration"))?;
        Ok(Arc::new(Self {
            host: node.host().trim_matches(['[', ']']).to_string(),
            port: node.port,
            name: config.server_name.clone().expect("validated JLS name"),
            crypto: crypto(config)?,
            owners: Mutex::default(),
            cached: tokio::sync::Mutex::new(None),
        }))
    }

    fn owner(&self, server: SocketAddr) -> Result<Arc<Owner>> {
        let mut owners = self.owners.lock();
        owners.endpoints.retain(|owner| owner.strong_count() != 0);
        if owners.closed {
            anyhow::bail!("ShadowQUIC client is closed");
        }
        if owners.endpoints.len() >= MAX_ENDPOINTS {
            return Err(super::super::PacketRejection::Capacity.into());
        }
        let endpoint = crate::runtime::new_owned_jls_endpoint(|| {
            let bind = if server.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            }
            .parse()
            .expect("literal socket");
            let socket = crate::util::marked_udp_socket(bind)?;
            quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                None,
                socket,
                Arc::new(quinn::TokioRuntime),
            )
        })?;
        let owner = Arc::new(Owner {
            endpoint,
            workers: Mutex::default(),
        });
        owners.endpoints.push(Arc::downgrade(&owner));
        Ok(owner)
    }

    pub async fn connect(&self, timeout: Duration) -> Result<Arc<Session>> {
        let operation = async {
            let pinned = crate::runtime::pinned_server_address(&self.host);
            let resolution = async {
                match pinned {
                    Some(ip) => Ok(vec![ip]),
                    None => crate::bootstrap::resolve(&self.host).await,
                }
            };
            let (resolved, selection) =
                crate::runtime::flow_observation::observe_resolution(resolution).await;
            let addresses: Vec<_> = resolved?
                .into_iter()
                .map(|ip| SocketAddr::new(ip, self.port))
                .collect();
            if addresses.is_empty() {
                anyhow::bail!("ShadowQUIC server resolved to no addresses");
            }
            let session = crate::address_race::race_resolved_addrs(&addresses, |server| {
                let selection = selection.as_ref();
                async move {
                    crate::runtime::admit_physical_dial(async {
                        let owner = self.owner(server)?;
                        let mut guard = PendingOwner(Some(Arc::clone(&owner)));
                        if let Some(selection) = selection {
                            selection.selected_ip(server.ip());
                        }
                        let conn = owner
                            .endpoint
                            .connect_with(self.crypto.clone(), server, &self.name)?
                            .await
                            .map_err(|error| super::super::NodeFailure(error.into()))?;
                        if conn.is_jls() != Some(true) {
                            return Err(super::super::NodeFailure(anyhow!(
                                "ShadowQUIC JLS authentication failed"
                            ))
                            .into());
                        }
                        if self.owners.lock().closed {
                            anyhow::bail!("ShadowQUIC client closed during handshake");
                        }
                        guard.0.take();
                        Ok::<Arc<Session>, anyhow::Error>(Arc::new(Session {
                            conn,
                            owner,
                            leases: Mutex::new(Leases {
                                active: 0,
                                last_used: tokio::time::Instant::now(),
                                retired: false,
                            }),
                        }))
                    })
                    .await
                }
            })
            .await
            .context("ShadowQUIC resolved no server address")??;
            attached(&session);
            Ok(session)
        };
        tokio::time::timeout(timeout, operation)
            .await
            .context("ShadowQUIC handshake deadline")?
    }

    pub async fn tcp(&self, timeout: Duration) -> Result<Lease> {
        let mut cached = self.cached.lock().await;
        if self.owners.lock().closed {
            anyhow::bail!("ShadowQUIC client is closed");
        }
        if let Some(lease) = cached.as_ref().and_then(Session::lease) {
            attached(&lease.0);
            return Ok(lease);
        }
        cached.take();
        let session = self.connect(timeout).await?;
        let lease = session
            .lease()
            .ok_or_else(|| anyhow!("ShadowQUIC session closed"))?;
        let weak = Arc::downgrade(&session);
        session.owner.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(session) = weak.upgrade() else { break };
                if session.reap_idle() {
                    break;
                }
            }
        })?;
        *cached = Some(session);
        Ok(lease)
    }
}

struct PendingOwner(Option<Arc<Owner>>);
impl Drop for PendingOwner {
    fn drop(&mut self) {
        if let Some(owner) = &self.0 {
            owner.abort();
        }
    }
}

#[async_trait::async_trait]
impl crate::runtime::QuicRuntimeClient for Client {
    fn into_erased(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }
    async fn force_close(&self) {
        let owners = {
            let mut state = self.owners.lock();
            state.closed = true;
            state
                .endpoints
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        for owner in &owners {
            owner.abort();
        }
        self.cached.lock().await.take();
        for owner in owners {
            owner.close().await;
        }
    }
    async fn release_warm(&self) {
        self.cached.lock().await.take();
    }
}

fn crypto(options: &ShadowQuicConfig) -> Result<quinn::ClientConfig> {
    let mut tls = quinn::rustls::ClientConfig::builder()
        .with_root_certificates(quinn::rustls::RootCertStore::empty())
        .with_no_client_auth();
    tls.jls_config = quinn::rustls::jls::JlsClientConfig::new(
        options.password.as_deref().expect("validated JLS password"),
        options.username.as_deref().expect("validated JLS username"),
    );
    tls.alpn_protocols = options
        .alpn
        .iter()
        .map(|name| name.as_bytes().to_vec())
        .collect();
    // Tickets may be negotiated, but no application data is sent before JLS verification.
    tls.enable_early_data = options.zero_rtt;
    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
    transport.stream_receive_window((1u32 << 20).into());
    transport.receive_window((4u32 << 20).into());
    transport.datagram_receive_buffer_size(Some(256 << 10));
    transport.datagram_send_buffer_size(256 << 10);
    transport.max_concurrent_uni_streams(16u32.into());
    if let Some(mtu) = options.mtu {
        transport.min_mtu(mtu.min(1290));
        transport.initial_mtu(mtu);
        transport.mtu_discovery_config(None);
    } else {
        transport.min_mtu(1290);
        transport.initial_mtu(1300);
    }
    match options.congestion.as_str() {
        "cubic" => {
            transport
                .congestion_controller_factory(Arc::new(quinn::congestion::CubicConfig::default()));
        }
        "new_reno" => {
            transport.congestion_controller_factory(Arc::new(
                quinn::congestion::NewRenoConfig::default(),
            ));
        }
        _ => {
            transport
                .congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
        }
    }
    config.transport_config(Arc::new(transport));
    Ok(config)
}

fn attached(session: &Session) {
    if let Some(observer) = crate::runtime::flow_observation::current() {
        observer.publish(
            crate::runtime::flow_observation::FlowEvent::TransportAttached {
                server_addr: Some(session.conn.remote_address()),
                resolution_location: crate::runtime::flow_observation::ResolutionLocation::Unknown,
            },
        );
    }
}
