use anyhow::Result;
use bytes::{Buf, Bytes};
use futures::io::{AsyncRead, AsyncWrite};
use futures::{Sink, Stream};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_tungstenite::tungstenite::Message;
use yamux::{Connection, Mode};

use super::forwarder::handle_tunnel_stream;

/// Wrapper to make WebSocket stream implement futures AsyncRead + AsyncWrite
pub struct WsCompat<S> {
    inner: S,
    read_buffer: VecDeque<Bytes>,
    closed: bool,
}

impl<S> WsCompat<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            read_buffer: VecDeque::new(),
            closed: false,
        }
    }
}

impl<S> Unpin for WsCompat<S> {}

impl<S> AsyncRead for WsCompat<S>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // First try buffer
        if let Some(mut data) = self.read_buffer.pop_front() {
            let len = std::cmp::min(data.len(), buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            data.advance(len);
            if !data.is_empty() {
                self.read_buffer.push_front(data);
            }
            return Poll::Ready(Ok(len));
        }

        if self.closed {
            return Poll::Ready(Ok(0));
        }

        let inner = Pin::new(&mut self.inner);
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(Message::Binary(data)))) => {
                let data = Bytes::from(data);
                let len = std::cmp::min(data.len(), buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                if len < data.len() {
                    self.read_buffer.push_back(data.slice(len..));
                }
                Poll::Ready(Ok(len))
            }
            Poll::Ready(Some(Ok(Message::Close(_)))) => {
                self.closed = true;
                Poll::Ready(Ok(0))
            }
            Poll::Ready(Some(Ok(_))) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string())))
            }
            Poll::Ready(None) => {
                self.closed = true;
                Poll::Ready(Ok(0))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> AsyncWrite for WsCompat<S>
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let inner = Pin::new(&mut self.inner);
        match inner.poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                let data = buf.to_vec();
                let len = data.len();
                let inner = Pin::new(&mut self.inner);
                match inner.start_send(Message::Binary(data)) {
                    Ok(()) => Poll::Ready(Ok(len)),
                    Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string()))),
                }
            }
            Poll::Ready(Err(e)) => {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string())))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let inner = Pin::new(&mut self.inner);
        match inner.poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string())))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let inner = Pin::new(&mut self.inner);
        match inner.poll_close(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e.to_string())))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub async fn run_tunnel(
    ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    local_addr: std::net::SocketAddr,
    local_host: Option<String>,
    forward_timeout: std::time::Duration,
    quiet: bool,
) -> Result<()> {
    let compat = WsCompat::new(ws);
    let config = yamux::Config::default();
    let mut connection = Connection::new(compat, config, Mode::Client);

    tracing::debug!("Tunnel established, waiting for requests...");

    // Accept incoming streams from server using poll_next_inbound
    loop {
        let result = std::future::poll_fn(|cx| connection.poll_next_inbound(cx)).await;
        match result {
            Some(Ok(stream)) => {
                let local_host = local_host.clone();
                tokio::spawn(async move {
                    handle_tunnel_stream(stream, local_addr, local_host, forward_timeout, quiet).await;
                });
            }
            Some(Err(e)) => {
                tracing::error!("Yamux error: {}", e);
                break;
            }
            None => {
                tracing::debug!("Connection closed");
                break;
            }
        }
    }

    Ok(())
}
