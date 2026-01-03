use axum::extract::ws::{Message, WebSocket};
use bytes::{Buf, Bytes};
use futures::io::{AsyncRead, AsyncWrite};
use futures::{Sink, Stream};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A compatibility wrapper that implements futures AsyncRead + AsyncWrite for WebSocket
pub struct Compat<S> {
    inner: S,
    read_buffer: VecDeque<Bytes>,
    closed: bool,
}

impl<S> Compat<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            read_buffer: VecDeque::new(),
            closed: false,
        }
    }
}

impl<S> Unpin for Compat<S> {}

impl AsyncRead for Compat<WebSocket> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // First, try to read from buffer
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

        // Poll the websocket for new messages
        let inner = Pin::new(&mut self.inner);
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(msg))) => match msg {
                Message::Binary(data) => {
                    let data = Bytes::from(data);
                    let len = std::cmp::min(data.len(), buf.len());
                    buf[..len].copy_from_slice(&data[..len]);
                    if len < data.len() {
                        self.read_buffer.push_back(data.slice(len..));
                    }
                    Poll::Ready(Ok(len))
                }
                Message::Close(_) => {
                    self.closed = true;
                    Poll::Ready(Ok(0))
                }
                _ => {
                    // Ignore text, ping, pong messages for yamux
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            },
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

impl AsyncWrite for Compat<WebSocket> {
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
