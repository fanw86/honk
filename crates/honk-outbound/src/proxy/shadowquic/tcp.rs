use super::client::Lease;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(super) struct Stream {
    send: quinn_jls::SendStream,
    recv: quinn_jls::RecvStream,
    lease: Lease,
}
impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowQuicStream").finish_non_exhaustive()
    }
}
impl Stream {
    pub fn new(send: quinn_jls::SendStream, recv: quinn_jls::RecvStream, lease: Lease) -> Self {
        Self { send, recv, lease }
    }
    fn classify(&self, error: io::Error) -> io::Error {
        if self.lease.0.conn.close_reason().is_some() {
            io::Error::new(error.kind(), super::super::NodeFailure(error.into()))
        } else {
            error
        }
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buffer)
            .map_err(|error| self.classify(error))
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buffer)
            .map_err(|error| self.classify(error))
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx).map_err(|error| self.classify(error))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
            .map_err(|error| self.classify(error))
    }
}
