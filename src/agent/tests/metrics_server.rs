use super::*;
use std::sync::atomic::AtomicBool;
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

    let provider = Arc::new(|| Arc::from("sample_metric 42\n"));
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

    let provider = Arc::new(|| Arc::from("sample_metric 42\n"));
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

    let provider = Arc::new(|| Arc::from("sample_metric 42\n"));
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

    let provider = Arc::new(|| Arc::from("sample_metric 42\n"));
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    let response = send_request(&addr, "POST /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn get_metrics_with_query_params_returns_200() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| Arc::from("sample_metric 42\n"));
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    let response = send_request(
        &addr,
        "GET /metrics?format=openmetrics HTTP/1.1\r\nHost: localhost\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("\r\n\r\nsample_metric 42\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn metrics_server_rejects_connections_exceeding_concurrency_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| Arc::from("metric 1\n"));
    let server_task = tokio::spawn(serve_listener_with_limit(listener, provider, shutdown.clone(), 1));

    // Connect first client (holds the single permit without sending anything yet)
    let client1 = TcpStream::connect(&addr).await.unwrap();

    // Give server a moment to accept client1 and consume the permit
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Connect second client while permit is occupied
    let mut client2 = TcpStream::connect(&addr).await.unwrap();
    let mut buf2 = Vec::new();
    client2.read_to_end(&mut buf2).await.unwrap();
    let response2 = String::from_utf8(buf2).unwrap();
    assert!(
        response2.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
        "expected 503, got: {response2}"
    );

    // Drop client1 to release the permit
    drop(client1);

    // Wait briefly for the task to finish and release the permit
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Connect third client: should now succeed with 200 OK
    let response3 = send_request(&addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response3.starts_with("HTTP/1.1 200 OK\r\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn malformed_request_without_newline_returns_400() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| Arc::from(""));
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    // 4096 bytes of 'A' without any newline
    let garbage = vec![b'A'; 4096];
    let mut stream = TcpStream::connect(&addr).await.unwrap();
    stream.write_all(&garbage).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let response = String::from_utf8(buf).unwrap();
    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_stops_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| Arc::from(""));
    let server_task = tokio::spawn(serve_listener(listener, provider, shutdown.clone()));

    shutdown.cancel();
    assert!(server_task.await.unwrap().is_ok());
}

#[tokio::test]
async fn get_readyz_returns_503_when_not_ready() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| Arc::from(""));
    let ready = Arc::new(AtomicBool::new(false));
    let server_task = tokio::spawn(serve_listener_with_ready(listener, provider, ready, shutdown.clone()));

    let response = send_request(&addr, "GET /readyz HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    assert!(response.ends_with("\r\n\r\nnot ready\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn get_readyz_returns_200_when_ready() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let shutdown = CancellationToken::new();

    let provider = Arc::new(|| Arc::from(""));
    let ready = Arc::new(AtomicBool::new(true));
    let server_task = tokio::spawn(serve_listener_with_ready(listener, provider, ready, shutdown.clone()));

    let response = send_request(&addr, "GET /readyz HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("\r\n\r\nready\n"));

    shutdown.cancel();
    server_task.await.unwrap().unwrap();
}
