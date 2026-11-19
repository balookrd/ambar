use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn send_request(addr: &str, req: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8(buf).unwrap()
}

#[tokio::test]
async fn get_metrics_returns_200_and_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| "sample_metric 42\n".to_string());
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    let response = send_request(&addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.contains("Content-Type: text/plain; version=0.0.4; charset=utf-8"));
    assert!(response.ends_with("\r\n\r\nsample_metric 42\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn get_root_returns_200_and_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| "sample_metric 42\n".to_string());
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    let response = send_request(&addr, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("\r\n\r\nsample_metric 42\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn get_unknown_path_returns_404() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| "sample_metric 42\n".to_string());
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    let response = send_request(&addr, "GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn post_returns_405() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| "sample_metric 42\n".to_string());
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    let response = send_request(&addr, "POST /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_stops_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| "".to_string());
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    shutdown.cancel();
    assert!(server_task.await.unwrap().is_ok());
}
