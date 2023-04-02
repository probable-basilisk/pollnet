use http::{HeaderMap, HeaderName, HeaderValue};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response};
use hyper_staticfile::Static;
use std::collections::HashMap;
use std::io::Error as IoError;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;

use super::*;

async fn handle_http_request<B>(
    req: Request<B>,
    static_: Option<Static>,
    virtual_files: Arc<RwLock<HashMap<String, Vec<u8>>>>,
) -> Result<Response<Body>, IoError> {
    debug!("HTTP req: {:}", req.uri().path());
    {
        // Do we need like... more headers???
        let vfiles = virtual_files.read().expect("RwLock poisoned");
        if let Some(file_data) = vfiles.get(req.uri().path()) {
            return Response::builder()
                .status(http::StatusCode::OK)
                .body(Body::from(file_data.clone()))
                .map_err(|_| IoError::new(std::io::ErrorKind::Other, "Rust errors are a pain"));
        }
    }

    match static_ {
        Some(static_) => static_.clone().serve(req).await,
        None => Response::builder()
            .status(http::StatusCode::NOT_FOUND)
            .body(Body::empty())
            .map_err(|_| IoError::new(std::io::ErrorKind::Other, "Rust errors are a pain")),
    }
}

async fn handle_get(
    url: String,
    headers: String,
    body_only: bool,
    dest: std::sync::mpsc::Sender<PollnetMessage>,
) {
    info!("HTTP GET: {}", url);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .headers(parse_headers(&headers))
        //.timeout(timeout)
        .send()
        .await;
    if handle_http_response(resp, body_only, dest).await.is_err() {
        warn!("HTTP GET socket closed at weird time.");
    }
}

async fn handle_post(
    url: String,
    headers: String,
    body: Vec<u8>,
    ret_body_only: bool,
    dest: std::sync::mpsc::Sender<PollnetMessage>,
) {
    info!("HTTP POST: {}", url);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .headers(parse_headers(&headers))
        .body(body)
        .send()
        .await;
    if handle_http_response(resp, ret_body_only, dest)
        .await
        .is_err()
    {
        warn!("HTTP POST socket closed at weird time.");
    }
}

fn parse_header_line(line: &str) -> Option<(HeaderName, &str)> {
    let (header_k, header_v) = line.split_once(':')?;
    let name = HeaderName::from_bytes(header_k.as_bytes()).ok()?;
    Some((name, header_v))
}

fn parse_headers(header_str: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for line in header_str.split('\n') {
        if line.is_empty() {
            continue;
        }
        let (name, val) = match parse_header_line(line) {
            Some(v) => v,
            None => {
                error!("Invalid header line: \"{:}\"", line);
                continue;
            }
        };
        if let Ok(val) = HeaderValue::from_str(val) {
            headers.insert(name, val);
        } else {
            error!("Invalid header value: \"{:}\"", val);
        }
    }
    headers
}

fn format_headers(header_map: &HeaderMap) -> String {
    let mut headers = String::new();
    for (key, value) in header_map.iter() {
        headers.push_str(key.as_ref());
        headers.push(':');
        headers.push_str(value.to_str().unwrap_or("MALFORMED"));
        headers.push('\n');
    }
    headers
}

async fn handle_http_response(
    resp: reqwest::Result<reqwest::Response>,
    ret_body_only: bool,
    dest: std::sync::mpsc::Sender<PollnetMessage>,
) -> anyhow::Result<()> {
    let resp = match resp {
        Ok(resp) => resp,
        Err(err) => {
            info!("HTTP failed: {}", err);
            send_error(dest, err);
            return Ok(());
        }
    };
    if !ret_body_only {
        let statuscode = resp.status().to_string();
        check_tx(dest.send(PollnetMessage::Binary(statuscode.into())))?;
        let headers = format_headers(resp.headers());
        check_tx(dest.send(PollnetMessage::Binary(headers.into())))?;
    };
    match resp.bytes().await {
        Ok(body) => {
            debug!("Body size: {:} bytes", body.len());
            check_tx(dest.send(PollnetMessage::Binary(body.to_vec())))?;
        }
        Err(body_err) => {
            debug!("Error getting HTTP body.");
            send_error(dest, body_err);
            return Ok(());
        }
    };
    debug!("HTTP request complete, sending disconnect.");
    send_disconnect(dest);
    Ok(())
}

impl PollnetContext {
    pub fn serve_http(&mut self, bind_addr: String, serve_dir: Option<String>) -> SocketHandle {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            info!("HTTP server spawned");
            let addr = bind_addr.parse();
            let addr = match addr {
                Ok(v) => v,
                Err(e) => {
                    error!("Invalid TCP address: {}", bind_addr);
                    send_error(tx_from_sock, format!("Invalid TCP address: {:?}", e));
                    return;
                }
            };

            let static_ = serve_dir.map(|path_string| Static::new(Path::new(&path_string)));

            let virtual_files: HashMap<String, Vec<u8>> = HashMap::new();
            let virtual_files = Arc::new(RwLock::new(virtual_files));
            let virtual_files_two_the_clone_wars = virtual_files.clone();

            let make_service = make_service_fn(|_| {
                // Rust demands all these clones for reasons I don't fully understand
                // I definitely feel so much safer though!
                let static_ = static_.clone();
                let virtual_files = virtual_files.clone();
                future::ok::<_, hyper::Error>(service_fn(move |req| {
                    handle_http_request(req, static_.clone(), virtual_files.clone())
                }))
            });

            let server = match hyper::Server::try_bind(&addr) {
                Err(bind_err) => {
                    error!("Couldn't bind {}: {}", bind_addr, bind_err);
                    send_error(tx_from_sock, bind_err);
                    return;
                }
                Ok(server) => server,
            }
            .serve(make_service);
            let graceful = server.with_graceful_shutdown(async move {
                let virtual_files = virtual_files_two_the_clone_wars.clone();
                loop {
                    match rx_to_sock.recv().await {
                        Some(PollnetMessage::Disconnect)
                        | Some(PollnetMessage::Error(_))
                        | None => break,
                        Some(PollnetMessage::FileAdd(filename, filedata)) => {
                            debug!("Adding virtual file: {:}", filename);
                            // I really do not see a reasonable scenario where
                            // this lock could end up poisoned so I think it's
                            // OK here to just panic.
                            let mut vfiles = virtual_files.write().expect("Lock is poisoned");
                            vfiles.insert(filename, filedata);
                        }
                        Some(PollnetMessage::FileRemove(filename)) => {
                            debug!("Removing virtual file: {:}", filename);
                            let mut vfiles = virtual_files.write().expect("Lock is poisoned");
                            vfiles.remove(&filename);
                        }
                        _ => {} // ignore sends?
                    }
                }
                info!("HTTP server trying to gracefully exit?");
            });
            info!("HTTP server running on http://{}/", addr);
            if let Err(err) = graceful.await {
                send_error(tx_from_sock, err);
            }
            info!("HTTP server stopped.");
        });

        let socket = Box::new(PollnetSocket {
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::Opening,
            data: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }

    pub fn open_http_get_simple(
        &mut self,
        url: String,
        headers: String,
        ret_body_only: bool,
    ) -> SocketHandle {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            let get_handler = handle_get(url, headers, ret_body_only, tx_from_sock);
            tokio::pin!(get_handler);
            loop {
                tokio::select! {
                    // handle_get will send the disconnect message after completion
                    _ = &mut get_handler => break,
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(PollnetMessage::Disconnect) | None => break,
                            _ => ()
                        }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket {
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::Opening,
            data: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }

    pub fn open_http_post_simple(
        &mut self,
        url: String,
        headers: String,
        body: Vec<u8>,
        ret_body_only: bool,
    ) -> SocketHandle {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            let post_handler = handle_post(url, headers, body, ret_body_only, tx_from_sock);
            tokio::pin!(post_handler);
            loop {
                tokio::select! {
                    _ = &mut post_handler => break,
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(PollnetMessage::Disconnect) | None => break,
                            _ => ()
                        }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket {
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::Opening,
            data: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }
}
