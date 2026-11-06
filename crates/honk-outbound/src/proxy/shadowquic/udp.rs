use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc};

use super::super::addr::SocksAddr;
use super::super::{FramePoison, MultiTargetTransport, PacketRejection, PacketTransport};
use super::client::Session;

const MAX_PAYLOAD: usize = 65507;
const MAX_CONTEXTS: usize = 256;
const MAX_STREAM_CONTEXTS: usize = 16;
const QUEUE: usize = 8;
const PENDING_TTL: Duration = Duration::from_secs(30);

struct Destination {
    address: SocksAddr,
    id: u16,
    stream: Option<quinn_jls::SendStream>,
}

struct Sender {
    control: quinn_jls::SendStream,
    destinations: Vec<Destination>,
}

pub(super) struct Udp {
    session: Arc<Session>,
    sender: Mutex<Sender>,
    poison: FramePoison,
    receiver: Mutex<mpsc::Receiver<(Bytes, SocketAddr)>>,
    target: SocketAddr,
    domain: Option<String>,
    over_stream: bool,
}

impl std::fmt::Debug for Udp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowQuicUdp").finish_non_exhaustive()
    }
}

impl Drop for Udp {
    fn drop(&mut self) {
        self.session.owner.abort();
    }
}

enum Event {
    Control(u16, Option<SocketAddr>),
    Data(u16, Bytes),
    Closed,
}

impl Udp {
    pub async fn new(
        session: Arc<Session>,
        target: SocketAddr,
        domain: Option<&str>,
        over_stream: bool,
    ) -> anyhow::Result<Arc<Self>> {
        if !over_stream && session.conn.max_datagram_size().is_none() {
            anyhow::bail!("ShadowQUIC peer did not negotiate datagrams");
        }
        let (mut send, recv) = session.conn.open_bi().await?;
        let mut header = vec![if over_stream { 0x04 } else { 0x03 }];
        SocksAddr::new("0.0.0.0:0".parse().expect("literal socket"), None)?.encode(&mut header);
        send.write_all(&header).await?;
        let (events, incoming) = mpsc::channel(QUEUE);
        let (output, receiver) = mpsc::channel(QUEUE);
        let connection = session.conn.clone();
        session.owner.spawn(async move {
            dispatch(connection, incoming, output).await;
        })?;
        let control_events = events.clone();
        session.owner.spawn(async move {
            let _ = read_control(recv, &control_events).await;
            let _ = control_events.send(Event::Closed).await;
        })?;
        let connection = session.conn.clone();
        let datagrams = events.clone();
        session.owner.spawn(async move {
            while let Ok(packet) = connection.read_datagram().await {
                if packet.len() < 2 || packet.len() > MAX_PAYLOAD + 2 {
                    continue;
                }
                let id = u16::from_be_bytes([packet[0], packet[1]]);
                if datagrams
                    .send(Event::Data(id, packet.slice(2..)))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })?;
        let connection = session.conn.clone();
        let owner = Arc::downgrade(&session.owner);
        session.owner.spawn(async move {
            let permits = Arc::new(tokio::sync::Semaphore::new(MAX_STREAM_CONTEXTS));
            while let Ok(stream) = connection.accept_uni().await {
                let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                    break;
                };
                let Some(owner) = owner.upgrade() else { break };
                let events = events.clone();
                if owner
                    .spawn(async move {
                        let _permit = permit;
                        if read_stream(stream, &events).await.is_err() {
                            let _ = events.send(Event::Closed).await;
                        }
                    })
                    .is_err()
                {
                    connection.close(0u32.into(), b"ShadowQUIC worker admission closed");
                    break;
                }
            }
        })?;
        Ok(Arc::new(Self {
            session,
            sender: Mutex::new(Sender {
                control: send,
                destinations: Vec::new(),
            }),
            poison: FramePoison::default(),
            receiver: Mutex::new(receiver),
            target,
            domain: domain.map(str::to_string),
            over_stream,
        }))
    }

    async fn send(
        &self,
        target: SocketAddr,
        domain: Option<&str>,
        payload: &[u8],
        admitted: Option<&AtomicBool>,
    ) -> io::Result<()> {
        self.send_inner(target, domain, payload, admitted)
            .await
            .map_err(|error| {
                if self.session.conn.close_reason().is_some()
                    && super::super::io_packet_rejection(&error).is_none()
                    && !super::super::is_frame_interrupted(&error)
                {
                    io::Error::new(error.kind(), super::super::NodeFailure(error.into()))
                } else {
                    error
                }
            })
    }

    async fn send_inner(
        &self,
        target: SocketAddr,
        domain: Option<&str>,
        payload: &[u8],
        admitted: Option<&AtomicBool>,
    ) -> io::Result<()> {
        if payload.len() > MAX_PAYLOAD {
            return Err(PacketRejection::InvalidSize.into());
        }
        if !self.over_stream
            && self
                .session
                .conn
                .max_datagram_size()
                .is_none_or(|max| payload.len() + 2 > max)
        {
            return Err(PacketRejection::InvalidSize.into());
        }
        let address = SocksAddr::new(target, domain)?;
        let mut sender = self.sender.lock().await;
        self.poison.check()?;
        let index = match sender
            .destinations
            .iter()
            .position(|destination| destination.address == address)
        {
            Some(index) => index,
            None => {
                let limit = if self.over_stream {
                    MAX_STREAM_CONTEXTS
                } else {
                    MAX_CONTEXTS
                };
                if sender.destinations.len() >= limit {
                    return Err(PacketRejection::Capacity.into());
                }
                let id = sender.destinations.len() as u16;
                let mut control = address.to_vec();
                control.extend_from_slice(&id.to_be_bytes());
                write_frame(&mut sender.control, &self.poison, &[&control], admitted).await?;
                sender.destinations.push(Destination {
                    address,
                    id,
                    stream: None,
                });
                id as usize
            }
        };
        let destination = &mut sender.destinations[index];
        if self.over_stream {
            if destination.stream.is_none() {
                let mut stream = self
                    .session
                    .conn
                    .open_uni()
                    .await
                    .map_err(io::Error::other)?;
                write_frame(
                    &mut stream,
                    &self.poison,
                    &[&destination.id.to_be_bytes()],
                    admitted,
                )
                .await?;
                destination.stream = Some(stream);
            }
            let stream = destination.stream.as_mut().expect("stream installed");
            write_frame(
                stream,
                &self.poison,
                &[&(payload.len() as u16).to_be_bytes(), payload],
                admitted,
            )
            .await?;
        } else {
            let mut packet = BytesMut::with_capacity(payload.len() + 2);
            packet.put_u16(destination.id);
            packet.extend_from_slice(payload);
            self.session
                .conn
                .send_datagram_wait(packet.freeze())
                .await
                .map_err(|error| match error {
                    quinn_jls::SendDatagramError::TooLarge => PacketRejection::InvalidSize.into(),
                    error => io::Error::other(super::super::NodeFailure(error.into())),
                })?;
            if let Some(admitted) = admitted {
                admitted.store(true, Ordering::Release);
            }
        }
        super::publish_request_sent();
        Ok(())
    }
}

#[async_trait]
impl PacketTransport for Udp {
    fn relay_addr(&self) -> SocketAddr {
        self.target
    }
    fn allows_full_cone_replies(&self) -> bool {
        true
    }
    fn send_timeout_is_congestion(&self) -> bool {
        !self.over_stream
    }
    async fn send_packet(&self, payload: &[u8]) -> io::Result<()> {
        self.send(self.target, self.domain.as_deref(), payload, None)
            .await
    }
    async fn recv_packet(&self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut receiver = self.receiver.lock().await;
        let (payload, peer) = receiver.recv().await.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "ShadowQUIC association closed",
            )
        })?;
        if payload.len() > buffer.len() {
            return Err(PacketRejection::InvalidSize.into());
        }
        buffer[..payload.len()].copy_from_slice(&payload);
        Ok((payload.len(), peer))
    }
}

#[async_trait]
impl MultiTargetTransport for Udp {
    async fn send_to(
        &self,
        target: SocketAddr,
        domain: Option<&str>,
        payload: &[u8],
        _confirmed: bool,
        admitted: Option<&AtomicBool>,
    ) -> io::Result<()> {
        self.send(target, domain, payload, admitted).await
    }
    fn carrier_addr(&self) -> Option<SocketAddr> {
        Some(self.session.conn.remote_address())
    }
    fn frame_stream_torn(&self) -> bool {
        self.poison.is_set()
    }
}

async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    poison: &FramePoison,
    parts: &[&[u8]],
    admitted: Option<&AtomicBool>,
) -> io::Result<()> {
    let frame = poison.begin(admitted)?;
    for part in parts {
        AsyncWriteExt::write_all(writer, part).await?;
    }
    frame.complete();
    Ok(())
}

async fn read_control(
    mut recv: quinn_jls::RecvStream,
    events: &mpsc::Sender<Event>,
) -> io::Result<()> {
    loop {
        let peer = super::super::addr::read_reply_source(
            &mut recv,
            None,
            "0.0.0.0:0".parse().expect("literal socket"),
        )
        .await?;
        let id = recv.read_u16().await?;
        if events.send(Event::Control(id, peer)).await.is_err() {
            return Ok(());
        }
    }
}

async fn read_stream(
    mut recv: quinn_jls::RecvStream,
    events: &mpsc::Sender<Event>,
) -> io::Result<()> {
    let id = tokio::time::timeout(Duration::from_secs(10), recv.read_u16())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "ShadowQUIC uni-stream header deadline",
            )
        })??;
    loop {
        let mut length = [0; 2];
        if AsyncReadExt::read(&mut recv, &mut length[..1]).await? == 0 {
            return Ok(());
        }
        AsyncReadExt::read_exact(&mut recv, &mut length[1..]).await?;
        let length = u16::from_be_bytes(length) as usize;
        if length > MAX_PAYLOAD {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut data = vec![0; length];
        AsyncReadExt::read_exact(&mut recv, &mut data).await?;
        if events
            .send(Event::Data(id, Bytes::from(data)))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

fn deliver(output: &mpsc::Sender<(Bytes, SocketAddr)>, peer: Option<SocketAddr>, payload: Bytes) {
    if let Some(peer) = peer {
        let _ = output.try_send((payload, peer));
    }
}

#[derive(Default)]
struct Replies {
    peers: HashMap<u16, Option<SocketAddr>>,
    pending: VecDeque<(tokio::time::Instant, u16, Bytes)>,
}

impl Replies {
    fn expire(&mut self) {
        while self
            .pending
            .front()
            .is_some_and(|entry| entry.0.elapsed() >= PENDING_TTL)
        {
            self.pending.pop_front();
        }
    }
    fn control(
        &mut self,
        id: u16,
        peer: Option<SocketAddr>,
        output: &mpsc::Sender<(Bytes, SocketAddr)>,
    ) -> io::Result<()> {
        self.expire();
        if self
            .peers
            .get(&id)
            .is_some_and(|previous| *previous != peer)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if !self.peers.contains_key(&id) && self.peers.len() >= MAX_CONTEXTS {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.peers.insert(id, peer);
        let mut remaining = VecDeque::new();
        while let Some(entry) = self.pending.pop_front() {
            if entry.1 == id {
                deliver(output, peer, entry.2);
            } else {
                remaining.push_back(entry);
            }
        }
        self.pending = remaining;
        Ok(())
    }
    fn data(&mut self, id: u16, payload: Bytes, output: &mpsc::Sender<(Bytes, SocketAddr)>) {
        self.expire();
        if let Some(peer) = self.peers.get(&id) {
            deliver(output, *peer, payload);
        } else {
            if self.pending.len() >= QUEUE {
                self.pending.pop_front();
            }
            self.pending
                .push_back((tokio::time::Instant::now(), id, payload));
        }
    }
}

async fn dispatch(
    conn: quinn_jls::Connection,
    mut events: mpsc::Receiver<Event>,
    output: mpsc::Sender<(Bytes, SocketAddr)>,
) {
    let mut replies = Replies::default();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = conn.closed() => break,
            _ = tick.tick() => replies.expire(),
            event = events.recv() => match event {
                Some(Event::Control(id, peer)) => { if replies.control(id, peer, &output).is_err() { break; } }
                Some(Event::Data(id, payload)) => replies.data(id, payload, &output),
                Some(Event::Closed) | None => break,
            }
        }
    }
    conn.close(0u32.into(), b"ShadowQUIC association receiver closed");
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn reference_session() -> (crate::runtime::OutboundRuntimeRegistry, Arc<crate::runtime::NodeRuntime>, Arc<Session>) {
        let server = std::env::var("HONK_SHADOWQUIC_TEST_SERVER").unwrap();
        let node = honk_config::node::Node::from_share_link(&format!("shadowquic://spike-user:spike-pass@{server}?sni=shadowquic.test#test")).unwrap();
        let registry = crate::runtime::OutboundRuntimeRegistry::build(std::slice::from_ref(&node)).unwrap();
        let runtime = registry.get(&node.id).unwrap();
        let session = runtime.scope_tasks(async {
            super::super::client::Client::new(&node)?.connect(Duration::from_secs(5)).await
        }).await.unwrap();
        (registry, runtime, session)
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires the pinned reference server"]
    async fn shadowquic_udp_construction_failure_aborts_started_workers() {
        let (registry, runtime, session) = reference_session().await;
        let owner = Arc::clone(&session.owner);
        let conn = session.conn.clone();
        runtime.scope_tasks(async {
            for _ in 0..super::super::client::MAX_WORKERS - 1 { owner.spawn(std::future::pending())?; }
            let result = Udp::new(session, "127.0.0.1:12345".parse().unwrap(), None, false).await;
            assert!(result.is_err());
            Ok(())
        }).await.unwrap();
        assert!(conn.close_reason().is_some());
        tokio::time::timeout(Duration::from_secs(5), registry.shutdown()).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the pinned reference server"]
    async fn shadowquic_udp_control_eof_closes_a_full_receive_queue() {
        let (registry, runtime, session) = reference_session().await;
        let echo = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            let mut buffer = [0; 64];
            while let Ok((length, peer)) = echo.recv_from(&mut buffer).await { let _ = echo.send_to(&buffer[..length], peer).await; }
        });
        let transport = runtime.scope_tasks(async { Udp::new(session, target, None, false).await }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while transport.receiver.lock().await.len() < QUEUE {
                transport.send_packet(b"queued").await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        transport.sender.lock().await.control.finish().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while transport.session.conn.close_reason().is_none() { tokio::time::sleep(Duration::from_millis(5)).await; }
        }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), registry.shutdown()).await.unwrap();
        echo_task.abort(); let _ = echo_task.await;
    }

    #[tokio::test]
    async fn cancelled_frame_poisons_the_association() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let poison = FramePoison::default();
        let admitted = AtomicBool::new(false);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                write_frame(
                    &mut writer,
                    &poison,
                    &[b"header", b"payload"],
                    Some(&admitted)
                )
            )
            .await
            .is_err()
        );
        assert!(admitted.load(Ordering::Acquire));
        assert!(poison.is_set());
        let error = write_frame(&mut writer, &poison, &[b"next"], None)
            .await
            .unwrap_err();
        assert!(super::super::super::is_frame_interrupted(&error));
    }

    #[tokio::test(start_paused = true)]
    async fn reordered_replies_are_bounded_expire_and_preserve_sources() {
        let (output, mut receiver) = mpsc::channel(QUEUE);
        let peer: SocketAddr = "127.0.0.2:1234".parse().unwrap();
        let mut replies = Replies::default();
        replies.data(7, Bytes::from_static(b"early"), &output);
        replies.control(7, Some(peer), &output).unwrap();
        assert_eq!(
            receiver.recv().await.unwrap(),
            (Bytes::from_static(b"early"), peer)
        );
        assert!(
            replies
                .control(7, Some("127.0.0.3:1234".parse().unwrap()), &output)
                .is_err()
        );
        replies.control(9, None, &output).unwrap();
        replies.data(9, Bytes::from_static(b"unprojectable-domain"), &output);
        assert!(receiver.try_recv().is_err());
        for id in 100..120 {
            replies.data(id, Bytes::from_static(b"bounded"), &output);
        }
        assert_eq!(replies.pending.len(), QUEUE);
        tokio::time::advance(PENDING_TTL + Duration::from_secs(1)).await;
        replies.expire();
        assert!(replies.pending.is_empty());
        for id in 0..MAX_CONTEXTS as u16 {
            if id != 7 && id != 9 {
                replies.control(id, Some(peer), &output).unwrap();
            }
        }
        assert!(
            replies
                .control(MAX_CONTEXTS as u16, Some(peer), &output)
                .is_err()
        );
    }
}
