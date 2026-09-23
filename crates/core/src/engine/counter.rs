//! 连接级字节计数。
//!
//! [`CountingStream`] 包在入站连接的最外层，所有经过该连接的字节都会被记账，
//! 与上层用的是 HTTP、CONNECT 隧道还是 SOCKS5 无关；TLS 入站时统计的是线路上的
//! 密文字节数，与网卡口径一致。
//!
//! 计数器与流分离持有：流被 hyper 或 `copy_bidirectional` 拿走后，调用方仍能在
//! 连接结束时读出结果。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

/// 一条连接的上下行字节计数，可克隆（内部共享同一组原子量）。
#[derive(Debug, Clone, Default)]
pub struct ByteCounters {
    up: Arc<AtomicU64>,
    down: Arc<AtomicU64>,
}

impl ByteCounters {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取当前累计值：`(上行, 下行)`，即 `(客户端→代理, 代理→客户端)`。
    pub fn get(&self) -> (u64, u64) {
        (
            self.up.load(Ordering::Relaxed),
            self.down.load(Ordering::Relaxed),
        )
    }
}

/// 透明转发读写并累加字节数的流包装。
#[derive(Debug)]
pub struct CountingStream<S> {
    inner: S,
    counters: ByteCounters,
}

impl<S> CountingStream<S> {
    pub fn new(inner: S, counters: ByteCounters) -> Self {
        Self { inner, counters }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CountingStream<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &result {
            let read = (buf.filled().len() - before) as u64;
            if read > 0 {
                self.counters.up.fetch_add(read, Ordering::Relaxed);
            }
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CountingStream<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = std::pin::Pin::new(&mut self.inner).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(n)) = &result {
            self.counters.down.fetch_add(*n as u64, Ordering::Relaxed);
        }
        result
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn counts_both_directions() {
        let (client, server) = tokio::io::duplex(64);
        let counters = ByteCounters::new();
        let mut counted = CountingStream::new(server, counters.clone());

        let mut client = client;
        client.write_all(b"hello").await.unwrap();

        let mut buf = [0u8; 5];
        counted.read_exact(&mut buf).await.unwrap();
        counted.write_all(b"world!").await.unwrap();

        assert_eq!(counters.get(), (5, 6));
    }
}
