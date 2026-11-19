//! Lightweight HTTP server for Prometheus metrics (R38).
//!
//! Serves `/metrics` over TCP using Tokio without external HTTP framework overhead.

use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// Maximum number of concurrent connections processed by the metrics server.
pub const MAX_CONCURRENT_METRICS_CONNECTIONS: usize = 32;

/// Serves Prometheus metrics on the given address until `shutdown` is triggered.
pub async fn run_metrics_server(
    addr: &str,
    provider: Arc<dyn Fn() -> String + Send + Sync>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    serve_listener(listener, provider, shutdown).await
}

pub(crate) async fn serve_listener(
    listener: TcpListener,
    provider: Arc<dyn Fn() -> String + Send + Sync>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    serve_listener_with_limit(listener, provider, shutdown, MAX_CONCURRENT_METRICS_CONNECTIONS).await
}

pub(crate) async fn serve_listener_with_limit(
    listener: TcpListener,
    provider: Arc<dyn Fn() -> String + Send + Sync>,
    shutdown: CancellationToken,
    concurrency_limit: usize,
) -> io::Result<()> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency_limit));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((mut stream, _)) => {
                        let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                            let resp = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                            let _ = stream.write_all(resp.as_bytes()).await;
                            let _ = stream.flush().await;
                            let _ = stream.shutdown().await;
                            continue;
                        };
                        let provider = provider.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let _ = tokio::time::timeout(
                                Duration::from_secs(5),
                                handle_connection(stream, provider),
                            )
                            .await;
                        });
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        if shutdown.is_cancelled() {
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

async fn handle_connection(mut stream: TcpStream, provider: Arc<dyn Fn() -> String + Send + Sync>) -> io::Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let raw_req = &buf[..n];
    if !raw_req.contains(&b'\n') && n == buf.len() {
        let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(resp.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    let req_str = String::from_utf8_lossy(raw_req);
    let first_line = req_str.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("");
    let path = raw_path.split('?').next().unwrap_or(raw_path);

    if method != "GET" {
        let resp = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(resp.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    if path != "/metrics" && path != "/" {
        let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(resp.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    let body = provider();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes()).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

#[cfg(test)]
#[path = "tests/metrics_server.rs"]
mod tests;
