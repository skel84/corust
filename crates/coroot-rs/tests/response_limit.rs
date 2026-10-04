//! `ClientBuilder::max_response_bytes` against a fake Coroot: every body the client reads
//! (data, error responses, MCP JSON and SSE) is bounded before it is decoded.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use coroot_rs::{Client, ClientBuilder, ErrorKind, Method};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// How the fake server sends a body.
#[derive(Clone)]
enum Body {
    /// With a `Content-Length` header.
    Sized(Vec<u8>),
    /// With chunked transfer encoding, in 1 KiB chunks.
    Chunked(Vec<u8>),
    /// Declares `Content-Length` but never sends the body: reading it would hang.
    Declared(u64),
    /// Sends chunks without ever ending the body: reading to the end would hang.
    Endless,
}

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Body,
}

impl Reply {
    fn json(status: u16, v: &Value) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Type", "application/json".into())],
            body: Body::Sized(v.to_string().into_bytes()),
        }
    }

    fn text(status: u16, body: impl Into<Vec<u8>>) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Type", "text/plain".into())],
            body: Body::Sized(body.into()),
        }
    }

    fn header(mut self, name: &'static str, value: &str) -> Reply {
        self.headers.push((name, value.into()));
        self
    }

    fn body(mut self, body: Body) -> Reply {
        self.body = body;
        self
    }
}

struct Request {
    path: String,
    body: Value,
}

type Handler = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

/// Starts a fake Coroot answering every request with `handler`; returns its URL.
async fn serve(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handler: Handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(handle(stream, handler.clone()));
        }
    });
    format!("http://{addr}")
}

async fn handle(mut stream: TcpStream, handler: Handler) {
    let mut buf = Vec::new();
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
    let reply = handler(&Request { path, body });

    let mut out = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    let chunked = matches!(reply.body, Body::Chunked(_) | Body::Endless);
    match &reply.body {
        Body::Sized(b) => out.push_str(&format!("Content-Length: {}\r\n", b.len())),
        Body::Declared(n) => out.push_str(&format!("Content-Length: {n}\r\n")),
        _ => out.push_str("Transfer-Encoding: chunked\r\n"),
    }
    out.push_str("\r\n");
    if stream.write_all(out.as_bytes()).await.is_err() {
        return;
    }
    let _ = match reply.body {
        Body::Sized(b) => stream.write_all(&b).await,
        Body::Chunked(b) => {
            for c in b.chunks(1024) {
                let mut frame = format!("{:x}\r\n", c.len()).into_bytes();
                frame.extend_from_slice(c);
                frame.extend_from_slice(b"\r\n");
                if stream.write_all(&frame).await.is_err() {
                    return;
                }
            }
            stream.write_all(b"0\r\n\r\n").await
        }
        Body::Declared(_) | Body::Endless => {
            if chunked {
                let frame = format!("400\r\n{}\r\n", "x".repeat(1024));
                for _ in 0..64 {
                    if stream.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                }
            }
            // Never finish the body.
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(())
        }
    };
}

fn builder(url: &str) -> ClientBuilder {
    // A short timeout turns "the client read past the limit" into a Network error.
    Client::builder(url)
        .api_key("crt_test")
        .timeout(Duration::from_secs(5))
}

/// A JSON document of exactly `n` bytes.
fn json_of_len(n: usize) -> Value {
    let v = json!({"pad": ""});
    let pad = n - v.to_string().len();
    let v = json!({"pad": "x".repeat(pad)});
    assert_eq!(v.to_string().len(), n);
    v
}

fn assert_too_large(e: &coroot_rs::Error, limit: u64, status: u16) {
    assert_eq!(e.kind(), ErrorKind::ResponseTooLarge, "{e}");
    assert_eq!(e.response_limit(), Some(limit));
    assert_eq!(e.http_status(), Some(status));
}

#[tokio::test]
async fn no_limit_by_default() {
    let doc = json_of_len(3 * 1024 * 1024);
    let reply = Reply::json(200, &doc);
    let url = serve(move |_| reply.clone()).await;
    let client = builder(&url).build().unwrap();
    assert_eq!(client.max_response_bytes(), None);
    assert_eq!(client.get_json("api/big", &[]).await.unwrap(), doc);
}

#[tokio::test]
async fn body_at_the_limit_is_read() {
    let doc = json_of_len(4096);
    let sized = Reply::json(200, &doc);
    let chunked = sized
        .clone()
        .body(Body::Chunked(doc.to_string().into_bytes()));
    let url = serve(move |r| {
        if r.path.contains("chunked") {
            chunked.clone()
        } else {
            sized.clone()
        }
    })
    .await;
    let client = builder(&url).max_response_bytes(4096).build().unwrap();
    assert_eq!(client.max_response_bytes(), Some(4096));
    assert_eq!(client.get_json("api/sized", &[]).await.unwrap(), doc);
    assert_eq!(client.get_json("api/chunked", &[]).await.unwrap(), doc);

    let client = builder(&url).max_response_bytes(4095).build().unwrap();
    let e = client.get_json("api/sized", &[]).await.unwrap_err();
    assert_too_large(&e, 4095, 200);
    let e = client.get_json("api/chunked", &[]).await.unwrap_err();
    assert_too_large(&e, 4095, 200);
}

#[tokio::test]
async fn declared_length_over_the_limit_is_rejected_without_reading() {
    let reply = Reply::json(200, &Value::Null).body(Body::Declared(10 * 1024 * 1024));
    let url = serve(move |_| reply.clone()).await;
    let client = builder(&url).max_response_bytes(1024).build().unwrap();
    let e = client.get_json("api/huge", &[]).await.unwrap_err();
    assert_too_large(&e, 1024, 200);
    assert!(e.message().contains("/api/huge"), "{e}");
}

#[tokio::test]
async fn unsized_body_is_cut_off_once_past_the_limit() {
    let reply = Reply::json(200, &Value::Null).body(Body::Endless);
    let url = serve(move |_| reply.clone()).await;
    let client = builder(&url).max_response_bytes(10 * 1024).build().unwrap();
    let e = client.get_json("api/stream", &[]).await.unwrap_err();
    assert_too_large(&e, 10 * 1024, 200);
}

#[tokio::test]
async fn typed_calls_are_bounded_and_nothing_is_decoded() {
    let reply = Reply::json(200, &json!({"context": {}, "data": json_of_len(8192)}));
    let url = serve(move |_| reply.clone()).await;
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    let client = builder(&url)
        .max_response_bytes(1024)
        .on_payload(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .unwrap();
    let e = client.project("p1").status().await.unwrap_err();
    assert_too_large(&e, 1024, 200);
    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "an oversized body reached the observer"
    );
}

#[tokio::test]
async fn error_responses_are_bounded() {
    let url = serve(|r| match r.path.as_str() {
        "/api/big-error" => Reply::text(500, "e".repeat(64 * 1024)),
        "/api/endless-error" => Reply::text(502, "").body(Body::Endless),
        "/api/denied" => Reply::text(401, ""),
        "/api/missing" => Reply::text(404, "application not found"),
        _ => Reply::text(500, "boom"),
    })
    .await;
    let client = builder(&url).max_response_bytes(1024).build().unwrap();

    let e = client.get_json("api/big-error", &[]).await.unwrap_err();
    assert_too_large(&e, 1024, 500);
    let e = client.get_json("api/endless-error", &[]).await.unwrap_err();
    assert_too_large(&e, 1024, 502);

    // Within the limit, errors keep their kinds.
    let e = client.get_json("api/denied", &[]).await.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Auth);
    let e = client.get_json("api/missing", &[]).await.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::NotFound);
    assert_eq!(e.message(), "application not found");
    let e = client.get_json("api/other", &[]).await.unwrap_err();
    assert_eq!((e.kind(), e.http_status()), (ErrorKind::Server, Some(500)));
}

#[tokio::test]
async fn send_json_and_read_body_are_bounded() {
    let url = serve(|r| match r.path.as_str() {
        "/api/small" => Reply::json(200, &json!({"ok": true})),
        _ => Reply::json(200, &json_of_len(4096)),
    })
    .await;
    let client = builder(&url).max_response_bytes(1024).build().unwrap();

    let e = client
        .send_json(Method::POST, "api/big", Some(&json!({"a": 1})))
        .await
        .unwrap_err();
    assert_too_large(&e, 1024, 200);
    let v = client
        .send_json(Method::POST, "api/small", None)
        .await
        .unwrap();
    assert_eq!(v, json!({"ok": true}));

    let resp = client
        .execute(client.request(Method::GET, "api/big").unwrap())
        .await
        .unwrap();
    let e = client.read_body(resp).await.unwrap_err();
    assert_too_large(&e, 1024, 200);
    let resp = client
        .execute(client.request(Method::GET, "api/small").unwrap())
        .await
        .unwrap();
    assert_eq!(client.read_body(resp).await.unwrap(), br#"{"ok":true}"#);
}

#[tokio::test]
async fn login_keeps_the_limit() {
    let url = serve(|r| match r.path.as_str() {
        "/api/login" => Reply::text(200, "").header("Set-Cookie", "coroot_session=s1; Path=/"),
        _ => Reply::json(200, &json_of_len(4096)),
    })
    .await;
    let client = Client::builder(&url)
        .max_response_bytes(1024)
        .login("a@example.com", "pw")
        .await
        .unwrap();
    assert_eq!(client.max_response_bytes(), Some(1024));
    let e = client.user().await.unwrap_err();
    assert_too_large(&e, 1024, 200);
}

/// A fake MCP endpoint: `initialize` and the initialized notification succeed, and
/// `tools/call` answers with `result` as JSON, or as SSE when `sse` is set.
fn mcp(result: Value, sse: Option<Body>) -> impl Fn(&Request) -> Reply + Send + Sync {
    move |r| {
        let id = r.body.get("id").cloned().unwrap_or(Value::Null);
        match r.body.get("method").and_then(Value::as_str) {
            Some("initialize") => Reply::json(
                200,
                &json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-03-26"}}),
            )
            .header("Mcp-Session-Id", "m1"),
            Some("tools/call") => {
                let text = result.to_string();
                let msg = json!({"jsonrpc": "2.0", "id": id,
                    "result": {"content": [{"type": "text", "text": text}]}});
                match &sse {
                    Some(body) => Reply {
                        status: 200,
                        headers: vec![("Content-Type", "text/event-stream".into())],
                        body: match body {
                            Body::Chunked(_) => Body::Chunked(
                                format!("event: message\ndata: {msg}\n\n").into_bytes(),
                            ),
                            other => other.clone(),
                        },
                    },
                    None => Reply::json(200, &msg),
                }
            }
            _ => Reply::text(202, ""),
        }
    }
}

#[tokio::test]
async fn mcp_responses_within_the_limit_work() {
    let url = serve(mcp(json!({"answer": 42}), None)).await;
    let client = builder(&url).max_response_bytes(4096).build().unwrap();
    let mut session = client.mcp().await.unwrap();
    let out = session.call_tool("t", json!({})).await.unwrap();
    assert_eq!(out.into_value(), json!({"answer": 42}));

    let url = serve(mcp(json!({"answer": 42}), Some(Body::Chunked(Vec::new())))).await;
    let client = builder(&url).max_response_bytes(4096).build().unwrap();
    let mut session = client.mcp().await.unwrap();
    let out = session.call_tool("t", json!({})).await.unwrap();
    assert_eq!(out.into_value(), json!({"answer": 42}));
}

#[tokio::test]
async fn mcp_responses_are_bounded() {
    let big = json_of_len(8192);

    let url = serve(mcp(big.clone(), None)).await;
    let client = builder(&url).max_response_bytes(4096).build().unwrap();
    let mut session = client.mcp().await.unwrap();
    let e = session.call_tool("t", json!({})).await.unwrap_err();
    assert_too_large(&e, 4096, 200);

    let url = serve(mcp(big.clone(), Some(Body::Chunked(Vec::new())))).await;
    let client = builder(&url).max_response_bytes(4096).build().unwrap();
    let mut session = client.mcp().await.unwrap();
    let e = session.call_tool("t", json!({})).await.unwrap_err();
    assert_too_large(&e, 4096, 200);

    let url = serve(mcp(big, Some(Body::Endless))).await;
    let client = builder(&url).max_response_bytes(4096).build().unwrap();
    let mut session = client.mcp().await.unwrap();
    let e = session.call_tool("t", json!({})).await.unwrap_err();
    assert_too_large(&e, 4096, 200);

    // The handshake is bounded too.
    let url = serve(|_| Reply::json(200, &json_of_len(8192))).await;
    let client = builder(&url).max_response_bytes(4096).build().unwrap();
    let e = client.mcp().await.unwrap_err();
    assert_too_large(&e, 4096, 200);
}
