//! Requests, scripted and real.

#[cfg(feature = "reqwest")]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(feature = "reqwest")]
use std::net::TcpListener;

use seam_core::io::ErrorKind;
use seam_reqwest::http::{Method, Response, StatusCode};
use seam_reqwest::{Scripted, get, get_text, post};

#[test]
fn scripted_routes_and_handlers() {
    let mut http = Scripted::new();
    http.respond("GET", "https://api.test/ok", 200, "fine")
        .respond("GET", "https://api.test/missing", 404, "")
        .on("POST", "https://api.test/echo", |req| {
            Ok(Response::builder()
                .status(StatusCode::CREATED)
                .body(req.body().to_ascii_uppercase())
                .unwrap())
        });
    assert_eq!(get_text(&mut http, "https://api.test/ok").unwrap(), "fine");
    assert_eq!(
        get(&mut http, "https://api.test/missing").unwrap().status(),
        404
    );
    assert_eq!(
        get_text(&mut http, "https://api.test/missing")
            .unwrap_err()
            .to_string(),
        "the server answered 404 Not Found"
    );
    let echoed = post(
        &mut http,
        "https://api.test/echo",
        "text/plain",
        b"shout".to_vec(),
    )
    .unwrap();
    assert_eq!(
        (echoed.status(), echoed.body().as_slice()),
        (StatusCode::CREATED, &b"SHOUT"[..])
    );
    let refused = get(&mut http, "https://elsewhere.test/").unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::ConnectionRefused);

    let requests = http.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        (requests[3].method.clone(), requests[3].body.clone()),
        (Method::POST, b"shout".to_vec())
    );

    // A later route replaces an earlier one.
    http.respond("GET", "https://api.test/ok", 200, "changed");
    assert_eq!(
        get_text(&mut http, "https://api.test/ok").unwrap(),
        "changed"
    );
}

#[cfg(feature = "test")]
#[test]
fn world_files_set_routes_and_check_requests() {
    use seam_reqwest::effects::Fetch;
    let mut root = seam_core::root![Scripted<Fetch>::new()];
    seam_test::load_world::<(Fetch,), _>(
        &mut root,
        r#"http { route "GET" "https://api.test/v" status=200 body="1.0" }"#,
    )
    .unwrap();
    assert_eq!(get_text(&mut root, "https://api.test/v").unwrap(), "1.0");
    seam_test::check_world::<(Fetch,), _>(&root, r#"http { request "GET" "https://api.test/v" }"#)
        .unwrap();
    assert!(
        seam_test::check_world::<(Fetch,), _>(
            &root,
            r#"http { request "POST" "https://api.test/v" }"#
        )
        .is_err()
    );
}

#[cfg(feature = "reqwest")]
/// Serves one request on a thread, answering with `body`. Returns the
/// URL and the thread, which returns the request line it read.
fn serve_once(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hello", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
            if line == "\r\n" {
                break;
            }
        }
        let mut body_in = vec![0; length];
        reader.read_exact(&mut body_in).unwrap();
        let mut stream = reader.into_inner();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nx-seam: yes\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        request_line.trim_end().to_owned()
    });
    (url, server)
}

#[cfg(feature = "reqwest")]
#[test]
fn real_requests_with_reqwest() {
    use seam_reqwest::{Reqwest, get_send, get_text_async};

    let (url, server) = serve_once("hi there");
    let response =
        seam_tokio::block_on(async { get_send(&mut Reqwest::new(), &url).await }).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-seam"], "yes");
    assert_eq!(response.body(), b"hi there");
    assert_eq!(server.join().unwrap(), "GET /hello HTTP/1.1");

    let (url, server) = serve_once("text");
    let text = seam_tokio::block_on(async { get_text_async(&mut Reqwest::new(), &url).await });
    assert_eq!(text.unwrap(), "text");
    server.join().unwrap();

    // Nothing listening.
    let refused =
        seam_tokio::block_on(async { get_send(&mut Reqwest::new(), "http://127.0.0.1:9/").await });
    assert_eq!(refused.unwrap_err().kind(), ErrorKind::ConnectionRefused);
}
