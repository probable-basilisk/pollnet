use http_body_util::Full;
use http_body_util::BodyExt;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener; // read_to_end()

use std::collections::HashMap;
use std::io::Error as IoError;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::RwLock;

use std::path::{Component, PathBuf};

use super::*;

type ReplyBody = Full<Bytes>;
type ReqResult = Result<Response<ReplyBody>, IoError>;

fn build_and_validate_path(base_path: &Path, requested_path: &str) -> Option<PathBuf> {
    // Largely taken from tower-http (MIT license): https://github.com/tower-rs/tower-http
    let path = Path::new(requested_path.trim_start_matches('/'));

    let mut full_path = base_path.to_path_buf();
    for component in path.components() {
        match component {
            Component::Normal(comp) => {
                // protect against paths like `/foo/c:/bar/baz` (#204)
                if Path::new(&comp)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
                {
                    full_path.push(comp)
                } else {
                    return None;
                }
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }
    Some(full_path)
}

async fn simple_file_send(basedir: &str, path: &str) -> ReqResult {
    let base = Path::new(basedir);
    let realpath = match build_and_validate_path(base, path) {
        Some(path) => path,
        None => {
            error!("ERROR: Malformed path: {:}", path);
            return not_found(path);
        }
    };

    let mut file = match File::open(realpath.as_path()).await {
        Ok(file) => file,
        Err(e) => {
            info!("404 for '{:}': {:}", realpath.display(), e);
            return not_found(path);
        }
    };

    let mut contents = vec![];
    if let Err(e) = file.read_to_end(&mut contents).await {
        error!("ERROR: Read issue on '{:}': {:}", realpath.display(), e);
        return not_found(path);
    };

    Ok(Response::new(Full::new(contents.into())))
}

fn not_found(_path: &str) -> ReqResult {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::new()))
        .map_err(|_| IoError::new(std::io::ErrorKind::Other, "Rust errors are a pain"))
}

async fn handle_http_request<B>(
    req: Request<B>,
    static_dir: Option<String>,
    virtual_files: Arc<RwLock<HashMap<String, Vec<u8>>>>,
) -> ReqResult {
    let path = req.uri().path();
    debug!("HTTP req: {:}", path);
    {
        // Do we need like... more headers???
        let vfiles = virtual_files.read().expect("RwLock poisoned");
        if let Some(file_data) = vfiles.get(req.uri().path()) {
            return Ok(Response::new(Full::new(file_data.clone().into())));
        }
    }

    match static_dir {
        Some(dir) => simple_file_send(&dir, path).await,
        None => not_found(path),
    }
}

fn parse_status(msg: Option<PollnetMessage>) -> StatusCode {
    match msg {
        Some(PollnetMessage::Text(txt)) => {
            StatusCode::from_str(&txt).ok()
        },
        Some(PollnetMessage::Binary(bin)) => {
            StatusCode::from_bytes(&bin).ok()
        },
        _ => {
            None
        }
    }.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn parse_headers_msg(msg: Option<PollnetMessage>) -> Vec<(String, String)> {
    let headerstr = match msg {
        Some(PollnetMessage::Binary(msg)) => {
            String::from_utf8_lossy(&msg).into_owned()
        },
        Some(PollnetMessage::Text(msg)) => {
            msg
        },
        _ => "".to_string()
    };
    parse_header_pairs(&headerstr)
}

fn parse_body(msg: Option<PollnetMessage>) -> Full<Bytes> {
    match msg {
        Some(PollnetMessage::Binary(bin)) => {
            Full::new(bin.into())
        },
        Some(PollnetMessage::Text(txt)) => {
            Full::new(txt.into())
        },
        _ => Full::new(Bytes::new())
    }
}

fn insert_header(headers: &mut hyper::HeaderMap, k: &str, v: &str) -> Option<()> {
    let k = hyper::header::HeaderName::from_bytes(k.as_bytes()).ok()?;
    let v = hyper::header::HeaderValue::from_str(v).ok()?;
    headers.insert(k, v);
    Some(())
}

async fn send_req_info(req: Request<hyper::body::Incoming>, tx: &mut std::sync::mpsc::Sender<PollnetMessage>) -> Option<()> {
    let req_method = req.method().as_str();
    let req_path = req.uri().to_string();
    let method_path = format!("{} {}", req_method, req_path);

    let req_headers = format_headers_2(req.headers());
    let req_body = req.collect().await.ok()?.to_bytes();

    tx.send(PollnetMessage::Text(method_path)).ok()?;
    tx.send(PollnetMessage::Text(req_headers)).ok()?;
    tx.send(PollnetMessage::Binary(req_body.to_vec())).ok()?;
    Some(())
}

async fn handle_dyn_http_request(
    req: Request<hyper::body::Incoming>,
    channels: Arc<tokio::sync::Mutex<ReactorChannels>>,
) -> ReqResult {
    let mut lock = channels.lock().await;

    if send_req_info(req, &mut lock.tx).await.is_none() {
        error!("Failed to send request info for some reason?");
    }

    let status = parse_status(lock.rx.recv().await);
    let hmap = parse_headers_msg(lock.rx.recv().await);
    let body = parse_body(lock.rx.recv().await);

    let mut resp = Response::builder().status(status);
    let headers = resp.headers_mut().unwrap();
    for (key, value) in hmap.iter() {
        insert_header(headers, key, value).unwrap_or_default()
    }

    resp.body(body)
        .map_err(|_| IoError::new(std::io::ErrorKind::Other, "Rust errors are a pain"))
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

fn parse_header_pairs(header_str: &str) -> Vec<(String, String)> {
    let mut res = Vec::new();
    for line in header_str.split('\n') {
        if line.is_empty() {
            continue;
        }
        let (header_k, header_v) = match line.split_once(':') {
            Some(p) => p,
            None => {
                error!("Invalid header line: \"{:}\"", line);
                continue
            }
        };
        res.push((header_k.to_string(), header_v.to_string()));
    }
    res
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

// yes these functions are identical but hyper and tungstenite are
// using two different versions of http (yay) so the actual types are slightly
// different and I can't be bothered to wrap this in a macro or something
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

fn format_headers_2(header_map: &hyper::HeaderMap) -> String {
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

async fn accept_http_tcp_dynamic(
    stream: TcpStream,
    addr: SocketAddr,
    outer_tx: std::sync::mpsc::Sender<PollnetMessage>,
) {
    let (host_io, reactor_io) = create_channels();
    let stream = TokioIo::new(stream);
    let reactor_io = Arc::new(tokio::sync::Mutex::new(reactor_io));

    if outer_tx
        .send(PollnetMessage::NewClient(ClientConn {
            io: host_io,
            id: addr.to_string(),
        }))
        .is_ok()
    {
        if let Err(err) = http1::Builder::new()
            .serve_connection(
                stream,
                service_fn(|req| handle_dyn_http_request(req, reactor_io.clone())),
            )
            .await
        {
            error!("Error serving connection: {:?}", err);
        }
    } else {
        warn!("TCP socket closed at weird time.");
    }

    // I guess we just won't close it...
}

async fn accept_http_tcp(
    stream: TcpStream,
    _addr: SocketAddr,
    _outer_tx: std::sync::mpsc::Sender<PollnetMessage>,
    static_dir: Option<String>,
    virtual_files: Arc<RwLock<HashMap<String, Vec<u8>>>>,
) {
    let io = TokioIo::new(stream);
    if let Err(err) = http1::Builder::new()
        .serve_connection(
            io,
            service_fn(|req| handle_http_request(req, static_dir.clone(), virtual_files.clone())),
        )
        .await
    {
        error!("Error serving connection: {:?}", err);
    }
}

impl PollnetContext {
    pub fn serve_http_dynamic(&mut self, addr: String) -> SocketHandle {
        let (host_io, mut reactor_io) = create_channels();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            let listener = match TcpListener::bind(&addr).await {
                Ok(listener) => listener,
                Err(tcp_err) => {
                    send_error(reactor_io.tx, tcp_err);
                    return;
                }
            };

            info!("DYN HTTP server running on http://{}/", addr);

            // We start a loop to continuously accept incoming connections
            loop {
                tokio::select! {
                    from_c_message = reactor_io.rx.recv() => {
                        match from_c_message {
                            Some(PollnetMessage::Disconnect)
                            | Some(PollnetMessage::Error(_))
                            | None => break,
                            _ => {} // ignore everything else
                        }
                    },
                    new_client = listener.accept() => {
                        match new_client {
                            Err(e) => {
                                error!("Client failed to connect: {:?}", e);
                            },
                            Ok((stream, addr)) => {
                                tokio::spawn(
                                    accept_http_tcp_dynamic(
                                        stream, addr, reactor_io.tx.clone()
                                    )
                                );
                            }
                        }
                    },
                };
            }
            info!("HTTP server stopped.");
        });

        let socket = Box::new(PollnetSocket {
            io: Some(host_io),
            status: SocketStatus::Opening,
            data: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }

    pub fn serve_http(&mut self, addr: String, serve_dir: Option<String>) -> SocketHandle {
        let (host_io, mut reactor_io) = create_channels();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            let listener = match TcpListener::bind(&addr).await {
                Ok(listener) => listener,
                Err(tcp_err) => {
                    send_error(reactor_io.tx, tcp_err);
                    return;
                }
            };

            let virtual_files: HashMap<String, Vec<u8>> = HashMap::new();
            let virtual_files = Arc::new(RwLock::new(virtual_files));
            let serve_dir2 = serve_dir.clone();

            info!("HTTP server running on http://{}/", addr);

            // We start a loop to continuously accept incoming connections
            loop {
                tokio::select! {
                    from_c_message = reactor_io.rx.recv() => {
                        match from_c_message {
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
                    },
                    new_client = listener.accept() => {
                        match new_client {
                            Err(e) => {
                                error!("Client failed to connect: {:?}", e);
                            },
                            Ok((stream, addr)) => {
                                tokio::spawn(
                                    accept_http_tcp(
                                        stream, addr, reactor_io.tx.clone(),
                                        serve_dir2.clone(), virtual_files.clone()
                                    )
                                );
                            }
                        }
                    },
                };
            }
            info!("HTTP server stopped.");
        });

        let socket = Box::new(PollnetSocket {
            io: Some(host_io),
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
        let (host_io, mut reactor_io) = create_channels();

        self.rt_handle.spawn(async move {
            let get_handler = handle_get(url, headers, ret_body_only, reactor_io.tx);
            tokio::pin!(get_handler);
            loop {
                tokio::select! {
                    // handle_get will send the disconnect message after completion
                    _ = &mut get_handler => break,
                    from_c_message = reactor_io.rx.recv() => {
                        match from_c_message {
                            Some(PollnetMessage::Disconnect) | None => break,
                            _ => ()
                        }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket {
            io: Some(host_io),
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
        let (host_io, mut reactor_io) = create_channels();

        self.rt_handle.spawn(async move {
            let post_handler = handle_post(url, headers, body, ret_body_only, reactor_io.tx);
            tokio::pin!(post_handler);
            loop {
                tokio::select! {
                    _ = &mut post_handler => break,
                    from_c_message = reactor_io.rx.recv() => {
                        match from_c_message {
                            Some(PollnetMessage::Disconnect) | None => break,
                            _ => ()
                        }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket {
            io: Some(host_io),
            status: SocketStatus::Opening,
            data: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }
}
