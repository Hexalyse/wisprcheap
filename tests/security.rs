//! Regressions at network and native-library trust boundaries.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{Router, http::StatusCode, response::IntoResponse, routing::post};

#[tokio::test]
async fn redirects_do_not_forward_provider_secrets_or_pairing_codes() {
    let received = Arc::new(AtomicUsize::new(0));
    let counter = received.clone();
    let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!("http://{}/capture", target_listener.local_addr().unwrap());
    let target_app = Router::new().route("/capture", post(move || async move {
        counter.fetch_add(1, Ordering::SeqCst);
        "captured"
    }));
    let target_task = tokio::spawn(async move { axum::serve(target_listener, target_app).await.unwrap() });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/v1/pair",
            post({
                let target = target.clone();
                move || async move { (StatusCode::TEMPORARY_REDIRECT, [("location", target)]).into_response() }
            }),
        )
        .route(
            "/provider",
            post(move || async move { (StatusCode::TEMPORARY_REDIRECT, [("location", target)]).into_response() }),
        );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let response = wisprcheap::llm::client()
        .post(format!("{base}/provider"))
        .header("xi-api-key", "test-secret")
        .body("private audio or text")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    let error = wisprcheap::sync::client::pair(
        &base,
        &wisprcheap_sync::protocol::PairRequest {
            code: "ABCD2345".into(),
            name: "test".into(),
            platform: "test".into(),
            app_version: "test".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        wisprcheap::sync::client::ApiError::Status { status: 307, .. }
    ));
    assert_eq!(
        received.load(Ordering::SeqCst),
        0,
        "redirect target must receive no credentials or payload"
    );
    task.abort();
    target_task.abort();
}

#[tokio::test]
async fn sync_rejects_oversized_responses_including_chunked_errors() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for status in ["200 OK", "500 Internal Server Error"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            socket.write_all(format!("HTTP/1.1 {status}\r\nTransfer-Encoding: chunked\r\n\r\n").as_bytes()).await.unwrap();
            let chunk = format!("2000\r\n{}\r\n", "x".repeat(8192));
            for _ in 0..257 {
                if socket.write_all(chunk.as_bytes()).await.is_err() { return; }
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });
        let error = wisprcheap::sync::client::Client::new(&base, "wcs_test").me().await.unwrap_err();
        assert_eq!(error.code(), Some("response_too_large"));
        server.await.unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn glib_string_iteration_regression() {
    use gtk::glib::prelude::*;
    let value = ["one", "deux", "三"].to_variant();
    assert_eq!(
        value.array_iter_str().unwrap().collect::<Vec<_>>(),
        ["one", "deux", "三"]
    );
    assert_eq!(
        value.array_iter_str().unwrap().rev().collect::<Vec<_>>(),
        ["三", "deux", "one"]
    );
    let mut iter = value.array_iter_str().unwrap();
    assert_eq!(iter.next(), Some("one"));
    assert_eq!(iter.next_back(), Some("三"));
    assert_eq!(iter.next(), Some("deux"));
    assert_eq!(iter.next_back(), None);
}
