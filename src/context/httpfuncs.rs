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

async fn _handle_get(url: String, body_only: bool, dest: std::sync::mpsc::Sender<SocketMessage>) {
    info!("HTTP GET: {}", url);
    _handle_http_response(reqwest::get(&url).await, body_only, dest).await;
}

async fn _handle_post(
    url: String,
    ret_body_only: bool,
    content_type: String,
    body: Vec<u8>,
    dest: std::sync::mpsc::Sender<SocketMessage>,
) {
    info!("HTTP POST: {} (w/ {})", url, content_type);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await;
    _handle_http_response(resp, ret_body_only, dest).await;
}

async fn _handle_http_response(
    resp: reqwest::Result<reqwest::Response>,
    body_only: bool,
    dest: std::sync::mpsc::Sender<SocketMessage>,
) {
    let resp = match resp {
        Ok(resp) => resp,
        Err(err) => {
            error!("HTTP failed: {}", err);
            dest.send(SocketMessage::Error(err.to_string()))
                .expect("TX error sending error!");
            return;
        }
    };
    if !body_only {
        let statuscode = resp.status().to_string();
        dest.send(SocketMessage::BinaryMessage(statuscode.into()))
            .expect("TX error on http status");
        let mut headers = String::new();
        for (key, value) in resp.headers().iter() {
            headers.push_str(key.as_ref());
            headers.push(':');
            headers.push_str(value.to_str().unwrap_or("MALFORMED"));
            headers.push_str(";\n");
        }
        dest.send(SocketMessage::BinaryMessage(headers.into()))
            .expect("TX error on http headers");
    };
    match resp.bytes().await {
        Ok(body) => {
            dest.send(SocketMessage::BinaryMessage(body.to_vec()))
                .expect("TX error on http body");
        }
        Err(body_err) => {
            dest.send(SocketMessage::Error(body_err.to_string()))
                .expect("TX error on http body error");
        }
    };
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
                Err(_) => {
                    error!("Invalid TCP address: {}", bind_addr);
                    tx_from_sock
                        .send(SocketMessage::Error("Invalid TCP address".to_string()))
                        .unwrap_or_default();
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

            let server = hyper::Server::try_bind(&addr);
            if let Err(bind_err) = server {
                error!("Couldn't bind {}: {}", bind_addr, bind_err);
                tx_from_sock
                    .send(SocketMessage::Error(bind_err.to_string()))
                    .unwrap_or_default();
                return;
            }
            let server = server.unwrap().serve(make_service);
            let graceful = server.with_graceful_shutdown(async move {
                let virtual_files = virtual_files_two_the_clone_wars.clone();
                loop {
                    match rx_to_sock.recv().await {
                        Some(SocketMessage::Disconnect) | Some(SocketMessage::Error(_)) | None => {
                            break
                        }
                        Some(SocketMessage::FileAdd(filename, filedata)) => {
                            let mut vfiles = virtual_files.write().expect("Lock is poisoned");
                            vfiles.insert(filename, filedata);
                        }
                        Some(SocketMessage::FileRemove(filename)) => {
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
                tx_from_sock
                    .send(SocketMessage::Error(err.to_string()))
                    .unwrap_or_default(); // don't care at this point
            }
            info!("HTTP server stopped.");
        });

        let socket = Box::new(PollnetSocket {
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::Opening,
            message: None,
            error: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }

    pub fn open_http_get_simple(&mut self, url: String, body_only: bool) -> SocketHandle {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            let get_handler = _handle_get(url, body_only, tx_from_sock);
            tokio::pin!(get_handler);
            loop {
                tokio::select! {
                    _ = &mut get_handler => break,
                    from_c_message = rx_to_sock.recv() => {
                        if let Some(SocketMessage::Disconnect) = from_c_message { break }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket {
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::Opening,
            message: None,
            error: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }
         
    pub fn open_http_post_simple(
        &mut self,
        url: String,
        ret_body_only: bool,
        content_type: String,
        body: Vec<u8>,
    ) -> SocketHandle {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            let post_handler = _handle_post(url, ret_body_only, content_type, body, tx_from_sock);
            tokio::pin!(post_handler);
            loop {
                tokio::select! {
                    _ = &mut post_handler => break,
                    from_c_message = rx_to_sock.recv() => {
                        if let Some(SocketMessage::Disconnect) = from_c_message { break }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket {
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::Opening,
            message: None,
            error: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }
}
