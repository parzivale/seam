//! HTTP for seam: a request is an effect.
//!
//! [`Fetch`] sends an `http::Request` and returns the whole
//! `http::Response`, bodies as bytes. A core that talks HTTP bounds on it,
//! and the edge decides where requests go:
//!
//! - [`Reqwest`] (feature `reqwest`, on by default): real requests, with a
//!   `reqwest::Client`, from async code under any executor: Tokio, smol,
//!   futures-rs'. Outside a Tokio runtime the requests run on one of this
//!   crate's (reqwest needs Tokio), started on first use;
//!   [`Reqwest::on`] picks the runtime.
//! - [`Scripted`]: answers from responses and handlers a test sets per
//!   method and URL, and records every request. With the `test` feature it
//!   reads and writes an `http` world-file block.
//!
//! Helpers build the common requests: [`get`], [`get_text`] and [`post`],
//! in sync, `_async` and `_send` forms (`Reqwest` is async only).
//!
//! # In tests
//! ```
//! use seam_reqwest::{Scripted, get_text};
//!
//! fn latest_version<C>(c: &mut C) -> seam_core::io::Result<String>
//! where
//!     C: seam_core::Perform<seam_reqwest::Fetch>,
//! {
//!     Ok(get_text(c, "https://example.com/version")?.trim().to_owned())
//! }
//!
//! let mut http = Scripted::new();
//! http.respond("GET", "https://example.com/version", 200, "1.4.2\n");
//! assert_eq!(latest_version(&mut http).unwrap(), "1.4.2");
//! assert_eq!(http.requests()[0].uri, "https://example.com/version");
//! ```
//! A world file (feature `test`) sets the routes, and records the requests
//! for `check_world` to check:
//!
//! ```text
//! http {
//!     route "GET" "https://example.com/version" status=200 body="1.4.2\n"
//! }
//! ```
//!
//! # In a program
//!
//! `Reqwest` is async only, so the core is async: the `_async` and `_send`
//! helpers, or `perform_send(Fetch(..))` for any request.
//!
//! ```no_run
//! # #[cfg(feature = "reqwest")] {
//! use seam_core::io;
//! use seam_reqwest::{Fetch, Reqwest, get_text_send};
//!
//! /// The core.
//! async fn latest_version<C>(c: &mut C) -> io::Result<String>
//! where
//!     C: seam_core::future::SendPerform<Fetch> + Send,
//! {
//!     Ok(get_text_send(c, "https://example.com/version").await?.trim().to_owned())
//! }
//!
//! // Under Tokio...
//! let mut http = Reqwest::new();
//! println!("{}", seam_tokio::block_on(latest_version(&mut http))?);
//! // ...or under smol, the same handler.
//! println!("{}", seam_smol::block_on(latest_version(&mut http))?);
//! # }
//! # Ok::<(), seam_core::io::Error>(())
//! ```
//!
//! A configured client (timeouts, proxies, headers) goes in with
//! [`Reqwest::with_client`]. In a root, `Reqwest` routes
//! [`effects::Fetch`].
//!
//! Through the `seam` crate, this crate is `seam::reqwest` (feature
//! `reqwest`, which brings in [`Reqwest`] with rustls; with `test`, the
//! `http` block).
//!
//! # Features
//!
//! | Feature | Default | Gives |
//! |---|---|---|
//! | `reqwest` | yes | [`Reqwest`] |
//! | `rustls` | yes | HTTPS for `Reqwest`, with rustls |
//! | `test` | no | `World` for [`Scripted`]: the `http` world-file block |
//!
#![cfg_attr(
    not(feature = "reqwest"),
    doc = "[`Reqwest`]: https://docs.rs/seam-rs-reqwest/latest/seam_reqwest/struct.Reqwest.html"
)]
#![cfg_attr(
    not(feature = "reqwest"),
    doc = "[`Reqwest::on`]: https://docs.rs/seam-rs-reqwest/latest/seam_reqwest/struct.Reqwest.html#method.on"
)]
#![cfg_attr(
    not(feature = "reqwest"),
    doc = "[`Reqwest::with_client`]: https://docs.rs/seam-rs-reqwest/latest/seam_reqwest/struct.Reqwest.html#method.with_client"
)]
#![forbid(unsafe_code)]

use std::boxed::Box;
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

pub use http;
use http::{Method, Request, Response, StatusCode};
use seam_core::io;
use seam_core::{Effect, Operation, Perform};

/// Send a request and wait for the whole response.
///
/// Fails with an `io::Error` when there is no response at all (the host
/// cannot be reached, the connection breaks, it times out); a response
/// with an error status (404, 500) is still a response.
#[derive(Debug)]
pub struct Fetch(pub Request<Vec<u8>>);

impl Operation for Fetch {
    type Outcome = io::Result<Response<Vec<u8>>>;
}

/// The keys that route this crate's effects in a root.
pub mod effects {
    /// Routes [`Fetch`](crate::Fetch).
    #[derive(Debug, Clone, Copy)]
    pub struct Fetch;
}

impl Effect for Fetch {
    type Key = effects::Fetch;
}

/// An error whose message is built at run time, so it travels as a
/// `std::io::Error`.
fn dynamic(kind: std::io::ErrorKind, e: impl std::fmt::Display) -> io::Error {
    io::Error::from(std::io::Error::new(kind, e.to_string()))
}

/// What a [`Scripted`] handler does with a request.
type Handler = Box<dyn FnMut(&Request<Vec<u8>>) -> io::Result<Response<Vec<u8>>> + Send>;

/// One route of a [`Scripted`] server.
struct Route {
    method: Method,
    uri: String,
    /// Set for routes made with `respond`, which a world file can show.
    #[cfg_attr(not(feature = "test"), allow(dead_code))]
    fixed: Option<(u16, Vec<u8>)>,
    handler: Handler,
}

/// A request [`Scripted`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// Its method.
    pub method: Method,
    /// Its URL, as sent.
    pub uri: String,
    /// Its body.
    pub body: Vec<u8>,
}

#[derive(Default)]
struct State {
    routes: Vec<Route>,
    requests: Vec<Recorded>,
}

/// Answers requests from routes a test sets, by method and exact URL, and
/// records every request. A request with no route fails with
/// `io::ErrorKind::ConnectionRefused`, as one to a host with nothing
/// listening would. A later route for the same method and URL replaces an
/// earlier one.
///
/// Every clone is a handle to the same routes and requests.
#[derive(Clone, Default)]
pub struct Scripted(Arc<Mutex<State>>);

impl core::fmt::Debug for Scripted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = self.lock();
        f.debug_struct("Scripted")
            .field("routes", &state.routes.len())
            .field("requests", &state.requests)
            .finish()
    }
}

/// `"GET"` or any other method name, as a `Method`.
fn method(name: &str) -> Method {
    Method::from_bytes(name.as_bytes()).expect("seam-reqwest: not an HTTP method")
}

impl Scripted {
    /// No routes yet.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add(&self, route: Route) {
        let mut state = self.lock();
        state
            .routes
            .retain(|r| !(r.method == route.method && r.uri == route.uri));
        state.routes.push(route);
    }

    /// Answers `method` requests to `uri` with `status` and `body`.
    ///
    /// # Panics
    ///
    /// If `method` is not a method name or `status` not a status code.
    pub fn respond(
        &mut self,
        method_name: &str,
        uri: &str,
        status: u16,
        body: impl Into<Vec<u8>>,
    ) -> &mut Self {
        let body = body.into();
        let code = StatusCode::from_u16(status).expect("seam-reqwest: not an HTTP status");
        let reply = body.clone();
        self.add(Route {
            method: method(method_name),
            uri: uri.into(),
            fixed: Some((status, body)),
            handler: Box::new(move |_| {
                Ok(Response::builder()
                    .status(code)
                    .body(reply.clone())
                    .expect("a status and a body make a response"))
            }),
        });
        self
    }

    /// Answers `method` requests to `uri` with `handler`, which sees the
    /// request.
    ///
    /// # Panics
    ///
    /// If `method` is not a method name.
    pub fn on(
        &mut self,
        method_name: &str,
        uri: &str,
        handler: impl FnMut(&Request<Vec<u8>>) -> io::Result<Response<Vec<u8>>> + Send + 'static,
    ) -> &mut Self {
        self.add(Route {
            method: method(method_name),
            uri: uri.into(),
            fixed: None,
            handler: Box::new(handler),
        });
        self
    }

    /// Every request received, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.lock().requests.clone()
    }
}

impl Perform<Fetch> for Scripted {
    fn perform(&mut self, op: Fetch) -> io::Result<Response<Vec<u8>>> {
        let request = op.0;
        let uri = request.uri().to_string();
        let mut state = self.lock();
        state.requests.push(Recorded {
            method: request.method().clone(),
            uri: uri.clone(),
            body: request.body().clone(),
        });
        let route = state
            .routes
            .iter_mut()
            .find(|r| r.method == request.method() && r.uri == uri)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "no scripted response for this request",
                )
            })?;
        (route.handler)(&request)
    }
}

impl seam_core::Capability for Scripted {}

/// `http { route "GET" "https://x/y" status=200 body="..."; request "GET"
/// "https://x/y" }`: loading adds the routes (`request` lines are ignored);
/// written out, it lists the fixed routes and every request received;
/// checking finds each expected route and request.
#[cfg(feature = "test")]
impl seam_test::World for Scripted {
    const BLOCKS: &'static [&'static str] = &["http"];

    fn load(&mut self, block: &seam_test::world_file::Node) -> seam_test::world_file::Result<()> {
        for child in block.block()? {
            match child.name() {
                "route" => {
                    child.no_children()?;
                    child.props_only(&["status", "body"])?;
                    child.arity(2, Some(2), "a method and a URL")?;
                    let method_name = child.args()[0].string("the method")?;
                    if Method::from_bytes(method_name.as_bytes()).is_err() {
                        return Err(child.args()[0]
                            .error(std::format!("`{method_name}` is not an HTTP method")));
                    }
                    let uri = child.args()[1].string("the URL")?;
                    let status = match child.prop("status") {
                        None => 200,
                        Some(v) => {
                            let status: u16 = v.uint("the status")?;
                            if StatusCode::from_u16(status).is_err() {
                                return Err(
                                    v.error(std::format!("`{status}` is not an HTTP status"))
                                );
                            }
                            status
                        }
                    };
                    let body = match child.prop("body") {
                        None => Vec::new(),
                        Some(v) => v.string("the body")?.as_bytes().to_vec(),
                    };
                    self.respond(method_name, uri, status, body);
                }
                "request" => {}
                _ => return Err(child.unknown("`http`", &["route", "request"])),
            }
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<seam_test::world_file::Node> {
        use seam_test::world_file::Node;
        let state = self.lock();
        let mut block = Node::new("http");
        for route in &state.routes {
            if let Some((status, body)) = &route.fixed {
                block = block.with_child(
                    Node::new("route")
                        .with_arg(route.method.as_str())
                        .with_arg(route.uri.as_str())
                        .with_prop("status", u32::from(*status))
                        .with_prop("body", String::from_utf8_lossy(body).into_owned()),
                );
            }
        }
        for request in &state.requests {
            block = block.with_child(
                Node::new("request")
                    .with_arg(request.method.as_str())
                    .with_arg(request.uri.as_str()),
            );
        }
        std::vec![block]
    }
}

#[cfg(feature = "reqwest")]
pub use real::{ClientBuilder, Reqwest};

// reqwest's configuration types, all pure. `Client`, `RequestBuilder` and
// `Response` perform requests: those are `Fetch`, through `Reqwest`.
#[cfg(feature = "reqwest")]
pub use reqwest::{Error, IntoUrl, NoProxy, Proxy, Url, dns, redirect, retry, tls};

#[cfg(feature = "reqwest")]
mod real {
    use super::*;

    /// Sends requests with a `reqwest::Client`. Async only:
    /// `AsyncPerform<Fetch>` and `SendPerform<Fetch>`, under any executor.
    ///
    /// reqwest needs a Tokio runtime. Inside one, requests run on it
    /// directly; outside one (smol, futures-rs' executors, `block_on`),
    /// they run on a small runtime of this crate's, one thread, started on
    /// first use. [`on`](Self::on) picks the runtime instead.
    #[derive(Debug, Clone, Default)]
    pub struct Reqwest {
        client: reqwest::Client,
        runtime: Option<tokio::runtime::Handle>,
    }

    impl Reqwest {
        /// A client with reqwest's defaults.
        pub fn new() -> Self {
            Self::default()
        }

        /// Sends with `client`, configured as wanted.
        pub fn with_client(client: reqwest::Client) -> Self {
            Self {
                client,
                runtime: None,
            }
        }

        /// A [`ClientBuilder`], to configure the client: timeouts,
        /// proxies, TLS, redirects, default headers, and the rest of
        /// reqwest's options.
        pub fn builder() -> ClientBuilder {
            ClientBuilder::new()
        }

        /// Always runs requests on `runtime`, wherever they are awaited.
        #[must_use]
        pub fn on(mut self, runtime: tokio::runtime::Handle) -> Self {
            self.runtime = Some(runtime);
            self
        }

        /// The client.
        pub fn client(&self) -> &reqwest::Client {
            &self.client
        }

        async fn send(&self, request: Request<Vec<u8>>) -> io::Result<Response<Vec<u8>>> {
            let runtime = match &self.runtime {
                Some(r) => r.clone(),
                None => match tokio::runtime::Handle::try_current() {
                    Ok(_) => return fetch(&self.client, request).await,
                    Err(_) => own_runtime()?,
                },
            };
            let client = self.client.clone();
            let mut task = Abort(runtime.spawn(async move { fetch(&client, request).await }));
            match (&mut task.0).await {
                Ok(out) => out,
                Err(e) => Err(dynamic(std::io::ErrorKind::Other, e)),
            }
        }
    }

    /// reqwest's [`ClientBuilder`](reqwest::ClientBuilder): every option,
    /// and [`build`](Self::build) gives a [`Reqwest`].
    #[derive(Debug, Default)]
    #[must_use]
    pub struct ClientBuilder(reqwest::ClientBuilder);

    impl ClientBuilder {
        /// reqwest's defaults.
        pub fn new() -> Self {
            Self(reqwest::ClientBuilder::new())
        }

        /// The configured client, as a handler.
        pub fn build(self) -> Result<Reqwest, reqwest::Error> {
            self.0.build().map(Reqwest::with_client)
        }
    }

    // Every option, forwarded: generated from reqwest's API.
    mod builder;

    /// This crate's runtime, for requests awaited outside one.
    fn own_runtime() -> io::Result<tokio::runtime::Handle> {
        static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
            std::sync::OnceLock::new();
        RUNTIME
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .thread_name("seam-reqwest")
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map(|r| r.handle().clone())
            .map_err(|e| dynamic(std::io::ErrorKind::Other, e.clone()))
    }

    /// A request on another runtime, stopped if its future is dropped.
    struct Abort<T>(tokio::task::JoinHandle<T>);

    impl<T> Drop for Abort<T> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    fn error(e: reqwest::Error) -> io::Error {
        let kind = if e.is_timeout() {
            std::io::ErrorKind::TimedOut
        } else if e.is_connect() {
            std::io::ErrorKind::ConnectionRefused
        } else if e.is_builder() {
            std::io::ErrorKind::InvalidInput
        } else {
            std::io::ErrorKind::Other
        };
        dynamic(kind, e)
    }

    async fn fetch(
        client: &reqwest::Client,
        request: Request<Vec<u8>>,
    ) -> io::Result<Response<Vec<u8>>> {
        let request = reqwest::Request::try_from(request).map_err(error)?;
        let response = client.execute(request).await.map_err(error)?;
        let mut out = Response::builder()
            .status(response.status())
            .version(response.version());
        if let Some(headers) = out.headers_mut() {
            headers.extend(
                response
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
        let body = response.bytes().await.map_err(error)?;
        out.body(body.to_vec())
            .map_err(|e| dynamic(std::io::ErrorKind::InvalidData, e))
    }

    impl seam_core::future::AsyncPerform<Fetch> for Reqwest {
        async fn perform_async(&mut self, op: Fetch) -> io::Result<Response<Vec<u8>>> {
            self.send(op.0).await
        }
    }

    impl seam_core::future::SendPerform<Fetch> for Reqwest {
        async fn perform_send(&mut self, op: Fetch) -> io::Result<Response<Vec<u8>>> {
            self.send(op.0).await
        }
    }
}

/// A request with no body.
fn bodiless(method: Method, uri: &str) -> io::Result<Request<Vec<u8>>> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Vec::new())
        .map_err(|e| dynamic(std::io::ErrorKind::InvalidInput, e))
}

/// The body of a successful response as text; an error for any other.
fn success_text(response: Response<Vec<u8>>) -> io::Result<String> {
    let status = response.status();
    if !status.is_success() {
        return Err(dynamic(
            std::io::ErrorKind::Other,
            std::format!("the server answered {status}"),
        ));
    }
    String::from_utf8(response.into_body())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the body is not valid UTF-8"))
}

/// Writes each helper in its sync, async and `Send` forms, from one body.
macro_rules! three_ways {
    ($(
        $(#[$m:meta])*
        pub fn $name:ident, $aname:ident, $sname:ident
        ($($args:tt)*) -> $ret:ty
        $body:block
    )*) => {$(
        $(#[$m])*
        pub fn $name<C>($($args)*) -> $ret
        where
            C: Perform<Fetch> + ?Sized,
        {
            #[allow(unused_macros)]
            macro_rules! fetch {
                ($c:expr, $r:expr) => { Perform::perform($c, Fetch($r)) };
            }
            $body
        }

        #[doc = concat!("`", stringify!($name), "`, for async capabilities.")]
        pub async fn $aname<C>($($args)*) -> $ret
        where
            C: seam_core::future::AsyncPerform<Fetch> + ?Sized,
        {
            #[allow(unused_macros)]
            macro_rules! fetch {
                ($c:expr, $r:expr) => { seam_core::future::AsyncPerform::perform_async($c, Fetch($r)).await };
            }
            $body
        }

        #[doc = concat!(
            "`", stringify!($name), "`, for async capabilities, with a `Send` future: ",
            "for multithreaded runtimes."
        )]
        pub async fn $sname<C>($($args)*) -> $ret
        where
            C: seam_core::future::SendPerform<Fetch> + Send + ?Sized,
        {
            #[allow(unused_macros)]
            macro_rules! fetch {
                ($c:expr, $r:expr) => { seam_core::future::SendPerform::perform_send($c, Fetch($r)).await };
            }
            $body
        }
    )*};
}

three_ways! {
    /// Sends a `GET` to `uri` and returns the response, whatever its
    /// status.
    pub fn get, get_async, get_send
    (c: &mut C, uri: &str) -> io::Result<Response<Vec<u8>>>
    {
        fetch!(c, bodiless(Method::GET, uri)?)
    }

    /// Sends a `GET` to `uri` and returns the body as text. Fails unless
    /// the status is a success (2xx), and with `InvalidData` if the body
    /// is not UTF-8.
    pub fn get_text, get_text_async, get_text_send
    (c: &mut C, uri: &str) -> io::Result<String>
    {
        success_text(fetch!(c, bodiless(Method::GET, uri)?)?)
    }

    /// Sends a `POST` of `body`, with the `Content-Type` `content_type`,
    /// to `uri`, and returns the response, whatever its status.
    pub fn post, post_async, post_send
    (c: &mut C, uri: &str, content_type: &str, body: Vec<u8>) -> io::Result<Response<Vec<u8>>>
    {
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(http::header::CONTENT_TYPE, content_type)
            .body(body)
            .map_err(|e| dynamic(std::io::ErrorKind::InvalidInput, e))?;
        fetch!(c, request)
    }
}
