//! `Reqwest` under every executor: Tokio's own runtime, smol, and
//! futures-rs' `block_on`, against a one-shot local HTTP server.
#![cfg(feature = "reqwest")]

use std::io::{Read, Write};
use std::net::TcpListener;

use seam_reqwest::{Reqwest, get_text_send};

/// Serves one request with `body`, on a thread; its URL.
fn serve(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 1024];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    url
}

#[test]
fn on_tokio() {
    let url = serve("tokio");
    let got = seam_tokio::block_on(async { get_text_send(&mut Reqwest::new(), &url).await });
    assert_eq!(got.unwrap(), "tokio");
}

#[test]
fn on_smol() {
    let url = serve("smol");
    let got = seam_smol::block_on(async { get_text_send(&mut Reqwest::new(), &url).await });
    assert_eq!(got.unwrap(), "smol");
}

#[test]
fn on_futures_block_on() {
    let url = serve("futures");
    let got = futures::executor::block_on(async { get_text_send(&mut Reqwest::new(), &url).await });
    assert_eq!(got.unwrap(), "futures");
}

#[test]
fn on_a_given_runtime() {
    let url = serve("given");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut http = Reqwest::new().on(runtime.handle().clone());
    let got = seam_smol::block_on(async { get_text_send(&mut http, &url).await });
    assert_eq!(got.unwrap(), "given");
}
