use std::future::{Future, poll_fn};
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Default)]
struct State {
    client_eof: AtomicBool,
    proxy_eof: AtomicBool,
    active: AtomicBool,
    client_error: AtomicBool,
}

struct Tracked<'a, T> { inner: &'a mut T, state: &'a State, client: bool }

impl<T> Tracked<'_, T> {
    fn error<E>(&self, result: Poll<io::Result<E>>) -> Poll<io::Result<E>> {
        if matches!(result, Poll::Ready(Err(_))) { self.state.client_error.store(self.client, Ordering::Relaxed); }
        result
    }
    fn written(&self, result: &Poll<io::Result<usize>>) {
        if matches!(result, Poll::Ready(Ok(n)) if *n != 0) { self.state.active.store(true, Ordering::Relaxed); }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Tracked<'_, T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let room = buf.remaining();
        let result = AsyncRead::poll_read(Pin::new(&mut *self.inner), cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && room != 0 {
            if buf.filled().len() == before {
                let eof = if self.client { &self.state.client_eof } else { &self.state.proxy_eof };
                eof.store(true, Ordering::Relaxed);
            } else { self.state.active.store(true, Ordering::Relaxed); }
        }
        self.error(result)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Tracked<'_, T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let result = AsyncWrite::poll_write(Pin::new(&mut *self.inner), cx, buf);
        self.written(&result); self.error(result)
    }
    fn poll_write_vectored(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        let result = AsyncWrite::poll_write_vectored(Pin::new(&mut *self.inner), cx, buf);
        self.written(&result); self.error(result)
    }
    fn is_write_vectored(&self) -> bool { self.inner.is_write_vectored() }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = AsyncWrite::poll_flush(Pin::new(&mut *self.inner), cx); self.error(result)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = AsyncWrite::poll_shutdown(Pin::new(&mut *self.inner), cx); self.error(result)
    }
}

pub(super) async fn copy<A, B>(client: &mut A, proxy: &mut B, timeout: Duration) -> Result<(u64, u64), super::RelayError>
where A: AsyncRead + AsyncWrite + Unpin, B: AsyncRead + AsyncWrite + Unpin {
    let state = State::default();
    let mut client = Tracked { inner: client, state: &state, client: true };
    let mut proxy = Tracked { inner: proxy, state: &state, client: false };
    let operation = tokio::io::copy_bidirectional_with_sizes(&mut client, &mut proxy, 8192, 8192);
    tokio::pin!(operation);
    let timer = tokio::time::sleep(timeout);
    tokio::pin!(timer);
    let mut started = false;
    poll_fn(|cx| {
        if let Poll::Ready(result) = operation.as_mut().poll(cx) {
            return Poll::Ready(result.map_err(|error| super::RelayError::new(error, state.client_error.load(Ordering::Relaxed))));
        }
        let active = state.active.swap(false, Ordering::Relaxed);
        if !timeout.is_zero() && (state.client_eof.load(Ordering::Relaxed) || state.proxy_eof.load(Ordering::Relaxed)) {
            if !started || active { timer.as_mut().reset(tokio::time::Instant::now() + timeout); started = true; }
            if timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(super::RelayError::new(
                    io::Error::new(io::ErrorKind::TimedOut, "ShadowQUIC half-closed stream inactivity timeout"),
                    !state.client_eof.load(Ordering::Relaxed),
                )));
            }
        }
        Poll::Pending
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn remote_fin_with_silent_local_client_is_bounded() {
        let (_local, mut client) = tokio::io::duplex(64);
        let (mut remote, mut proxy) = tokio::io::duplex(64);
        remote.shutdown().await.unwrap();
        let result = copy(&mut client, &mut proxy, Duration::from_millis(50)).await;
        assert_eq!(result.unwrap_err().error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_disables_half_close_timeout() {
        let (mut local, mut client) = tokio::io::duplex(64);
        let (mut remote, mut proxy) = tokio::io::duplex(64);
        remote.shutdown().await.unwrap();
        let operation = tokio::spawn(async move { copy(&mut client, &mut proxy, Duration::ZERO).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1000)).await;
        assert!(!operation.is_finished());
        local.shutdown().await.unwrap();
        operation.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn accepted_buffered_writes_reset_inactivity_after_fin() {
        let (mut local, mut client) = tokio::io::duplex(64);
        let (mut remote, mut proxy) = tokio::io::duplex(1);
        remote.shutdown().await.unwrap();
        local.write_all(b"buffered").await.unwrap();
        let operation = tokio::spawn(async move { copy(&mut client, &mut proxy, Duration::from_millis(100)).await });
        let started = tokio::time::Instant::now();
        for _ in 0..8 {
            let mut byte = [0];
            remote.read_exact(&mut byte).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(!operation.is_finished());
        }
        assert!(started.elapsed() > Duration::from_millis(100));
        local.shutdown().await.unwrap();
        operation.await.unwrap().unwrap();
    }

    struct StalledShutdown;
    impl AsyncRead for StalledShutdown {
        fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, _: &mut ReadBuf<'_>) -> Poll<io::Result<()>> { Poll::Pending }
    }
    impl AsyncWrite for StalledShutdown {
        fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> { Poll::Ready(Ok(buf.len())) }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Pending }
    }
    #[tokio::test(start_paused = true)]
    async fn eof_starts_deadline_before_shutdown_completes() {
        let (mut local, mut client) = tokio::io::duplex(64);
        local.shutdown().await.unwrap();
        let result = copy(&mut client, &mut StalledShutdown, Duration::from_millis(50)).await;
        assert_eq!(result.unwrap_err().error.kind(), io::ErrorKind::TimedOut);
    }

    #[cfg(feature = "shadowquic")]
    #[tokio::test]
    #[ignore = "requires the pinned reference server"]
    async fn shadowquic_relay_bounds_remote_fin_with_silent_client() {
        use honk_outbound::proxy::{TcpOutbound, shadowquic::ShadowQuicHandler};
        use std::sync::{Arc, atomic::AtomicU64};
        let server = std::env::var("HONK_SHADOWQUIC_TEST_SERVER").unwrap();
        let node = honk_config::node::Node::from_share_link(&format!("shadowquic://spike-user:spike-pass@{server}?sni=shadowquic.test&half-close-timeout=50#test")).unwrap();
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = target_listener.local_addr().unwrap();
        let target_task = tokio::spawn(async move {
            let (mut socket, _) = target_listener.accept().await.unwrap();
            socket.shutdown().await.unwrap();
            let mut data = Vec::new();
            let _ = socket.read_to_end(&mut data).await;
        });
        let proxy = ShadowQuicHandler::new().dial(&node, target, None, Duration::from_secs(5)).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut local = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (mut client, client_addr) = listener.accept().await.unwrap();
        let progress = crate::relay::RelayProgress {
            upload: Arc::new(AtomicU64::new(0)), download: Arc::new(AtomicU64::new(0)),
            outbound_upload: None, outbound_download: None, first_response: None, on_transfer: None,
        };
        let timeout = Duration::from_millis(node.shadowquic().unwrap().half_close_timeout.unwrap());
        let relay = tokio::spawn(async move {
            crate::relay::relay_proxy_with_half_close(&mut client, proxy, client_addr, target, progress, Some(timeout)).await
        });
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), local.read(&mut [0; 1])).await.unwrap().unwrap(), 0);
        let result = tokio::time::timeout(Duration::from_secs(2), relay).await.unwrap().unwrap();
        assert!(result.is_err());
        drop(local);
        tokio::time::timeout(Duration::from_secs(5), target_task).await.unwrap().unwrap();
    }
}
