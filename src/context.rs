use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::Arc;
use std::thread;
use std::net::SocketAddr;
use std::io::Error as IoError;
use std::path::Path;
use log::{error, warn, info};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::{connect_async, accept_async};
use futures::executor::block_on;
use futures_util::{SinkExt, StreamExt, future};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response};
use hyper_staticfile::Static;

#[derive(Debug)]
pub enum RecvError {
    Empty,
    Disconnected,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum SocketResult {
    INVALIDHANDLE,
    CLOSED,
    OPENING,
    NODATA,
    HASDATA,
    ERROR,
    NEWCLIENT,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum SocketStatus {
    INVALIDHANDLE,
    CLOSED,
    OPEN,
    OPENING,
    ERROR,
}

struct ClientConn {
    tx: tokio::sync::mpsc::Sender<SocketMessage>, 
    rx: std::sync::mpsc::Receiver<SocketMessage>, 
    id: String,
}


enum SocketMessage {
    Connect,
    Disconnect,
    Message(String),
    BinaryMessage(Vec<u8>),
    Error(String),
    NewClient(ClientConn),
    FileAdd(String, Vec<u8>),
    FileRemove(String),
}

pub struct PollnetSocket {
    pub status: SocketStatus,
    tx: tokio::sync::mpsc::Sender<SocketMessage>,
    rx: std::sync::mpsc::Receiver<SocketMessage>,
    pub message: Option<Vec<u8>>,
    pub error: Option<String>,
    pub last_client_handle: u32,
}

pub struct PollnetContext {
    pub sockets: HashMap<u32, Box<PollnetSocket>>,
    next_handle: u32,
    thread: Option<thread::JoinHandle<()>>,
    rt_handle: tokio::runtime::Handle,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<i32>>,
}

async fn accept_ws(tcp_stream: TcpStream, addr: SocketAddr, outer_tx: std::sync::mpsc::Sender<SocketMessage>) {//rx_to_sock: tokio::sync::mpsc::Receiver<SocketMessage>, tx_from_sock: std::sync::mpsc::Sender<SocketMessage>) {
    let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
    let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

    outer_tx.send(SocketMessage::NewClient(ClientConn{
        tx: tx_to_sock,
        rx: rx_from_sock,
        id: addr.to_string(), //"BLURGH".to_string(),
    })).expect("this shouldn't ever break?");

    match accept_async(tcp_stream).await {
        Ok(mut ws_stream) => {
            tx_from_sock.send(SocketMessage::Connect).expect("oh boy");
            loop {
                tokio::select! {
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(SocketMessage::Message(msg)) => {
                                ws_stream.send(tungstenite::protocol::Message::Text(msg)).await.expect("WS send error");
                            },
                            Some(SocketMessage::BinaryMessage(msg)) => {
                                ws_stream.send(tungstenite::protocol::Message::Binary(msg)).await.expect("WS send error");
                            },
                            _ => break
                        }
                    },
                    from_sock_message = ws_stream.next() => {
                        match from_sock_message {
                            Some(Ok(msg)) => {
                                tx_from_sock.send(SocketMessage::BinaryMessage(msg.into_data())).expect("TX error on socket message");
                            },
                            Some(Err(msg)) => {
                                tx_from_sock.send(SocketMessage::Error(msg.to_string())).expect("TX error on socket error");
                                break;
                            },
                            None => {
                                tx_from_sock.send(SocketMessage::Disconnect).expect("TX error on disconnect");
                                break;
                            }
                        }
                    },
                };
            }
        },
        Err(err) => {
            error!("connection error: {}", err);
            tx_from_sock.send(SocketMessage::Error(err.to_string())).expect("TX error on connection error");
        }
    }
}

async fn accept_tcp(mut tcp_stream: TcpStream, addr: SocketAddr, outer_tx: Option<std::sync::mpsc::Sender<SocketMessage>>) {
    let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
    let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

    if let Some(tx) = outer_tx {
        tx.send(SocketMessage::NewClient(ClientConn{
            tx: tx_to_sock,
            rx: rx_from_sock,
            id: addr.to_string(),
        })).expect("this shouldn't ever break?");
    }

    tx_from_sock.send(SocketMessage::Connect).expect("oh boy");
    let mut buf = [0; 65536];
    loop {
        tokio::select! {
            from_c_message = rx_to_sock.recv() => {
                match from_c_message {
                    Some(SocketMessage::Message(msg)) => {
                        tcp_stream.write_all(msg.as_bytes()).await.expect("TCP send error");
                    },
                    Some(SocketMessage::BinaryMessage(msg)) => {
                        tcp_stream.write_all(&msg).await.expect("TCP send error");
                    },
                    _ => break
                }
            },
            _ = tcp_stream.readable() => {
                match tcp_stream.try_read(&mut buf){
                    Ok(n) => {
                        // TODO: can we avoid these copies? Does it matter?
                        let submessage = buf[0..n].to_vec();
                        tx_from_sock.send(SocketMessage::BinaryMessage(submessage)).expect("TX error on socket message");
                    }
                    Err(ref e) if e.kind() == tokio::io::ErrorKind::WouldBlock => {
                        // no effect?
                    }
                    Err(err) => {
                        tx_from_sock.send(SocketMessage::Error(err.to_string())).expect("TX error on socket error");
                        break;
                    }
                }
            },
        };
    }
    info!("Closing TCP socket!");
    tcp_stream.shutdown().await.unwrap_or_default(); // if this errors we don't care
}

async fn handle_http_request<B>(req: Request<B>, static_: Option<Static>, virtual_files: Arc<RwLock<HashMap<String, Vec<u8>>>>) -> Result<Response<Body>, IoError> {
    {
        // Do we need like... more headers???
        let vfiles = virtual_files.read().expect("RwLock poisoned");
        if let Some(file_data) = vfiles.get(req.uri().path()) {
            return Response::builder()
                    .status(http::StatusCode::OK)
                    .body(Body::from(file_data.clone()))
                    .map_err(|_| IoError::new(std::io::ErrorKind::Other, "Rust errors are a pain"))
        }
    }

    match static_ {
        Some(static_) => static_.clone().serve(req).await,
        None => {
            Response::builder().status(http::StatusCode::NOT_FOUND).body(Body::empty()).map_err(|_| IoError::new(std::io::ErrorKind::Other, "Rust errors are a pain"))
        }
    }
}

async fn _handle_http_response(resp: reqwest::Result<reqwest::Response>, body_only: bool, dest: std::sync::mpsc::Sender<SocketMessage>) {
    let resp = match resp {
        Ok(resp) => resp,
        Err(err) => {
            error!("HTTP failed: {}", err);
            dest.send(SocketMessage::Error(err.to_string())).expect("TX error sending error!");
            return;
        }
    };
    if !body_only {
        let statuscode = resp.status().to_string();
        dest.send(SocketMessage::BinaryMessage(statuscode.into())).expect("TX error on http status");
        let mut headers = String::new();
        for (key, value) in resp.headers().iter() {
            headers.push_str(&key.to_string());
            headers.push_str(":");
            headers.push_str(value.to_str().unwrap_or("MALFORMED"));
            headers.push_str(";\n");
        }
        dest.send(SocketMessage::BinaryMessage(headers.into())).expect("TX error on http headers");
    };
    match resp.bytes().await {
        Ok(body) => {
            dest.send(SocketMessage::BinaryMessage(body.to_vec())).expect("TX error on http body");
        },
        Err(body_err) => {
            dest.send(SocketMessage::Error(body_err.to_string())).expect("TX error on http body error");
        }
    };
}

impl PollnetContext {
    pub fn new() -> PollnetContext {
        match env_logger::try_init() {
            Err(err) => warn!("Multiple contexts created!: {}", err),
            _ => (),
        }

        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let shutdown_tx = Some(shutdown_tx);

        let thread = Some(thread::spawn(move || {
            let rt = runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Unable to create the runtime");

            // Send handle back out so we can store it?
            handle_tx
                .send(rt.handle().clone())
                .expect("Unable to give runtime handle to another thread");

            // Continue running until notified to shutdown
            info!("tokio runtime starting");
            rt.block_on(async {
                shutdown_rx.await.unwrap();
                // uh let's just put in a 'safety' delay to shut everything down?
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            });
            rt.shutdown_timeout(std::time::Duration::from_millis(200));
            info!("tokio runtime shutdown");
        }));

        PollnetContext{
            next_handle: 1,
            rt_handle: handle_rx.recv().unwrap(),
            thread: thread,
            shutdown_tx: shutdown_tx,
            sockets: HashMap::new()
        }
    }

    fn _next_handle_that_satisfies_the_borrow_checker(next_handle: &mut u32) -> u32 {
        let new_handle: u32 = *next_handle;
        *next_handle += 1;
        new_handle   
    }

    fn _next_handle(&mut self) -> u32 {
        PollnetContext::_next_handle_that_satisfies_the_borrow_checker(&mut self.next_handle)
    }

    pub fn serve_http(&mut self, bind_addr: String, serve_dir: Option<String>) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            info!("HTTP server spawned");
            let addr = bind_addr.parse();
            if let Err(_) = addr {
                error!("Invalid TCP address: {}", bind_addr);
                tx_from_sock.send(SocketMessage::Error("Invalid TCP address".to_string())).unwrap_or_default();
                return;
            }
            let addr = addr.unwrap();

            let static_ = match serve_dir {
                Some(path_string) => Some(Static::new(Path::new(&path_string))),
                None => None
            };

            let virtual_files: HashMap<String, Vec<u8>> = HashMap::new();
            let virtual_files = Arc::new(RwLock::new(virtual_files));
            let virtual_files_two_the_clone_wars = virtual_files.clone();

            let make_service = make_service_fn(|_| {
                // Rust demands all these clones for reasons I don't fully understand
                // I definitely feel so much safer though!
                let static_ = static_.clone();
                let virtual_files = virtual_files.clone();
                future::ok::<_, hyper::Error>(service_fn(move |req| handle_http_request(req, static_.clone(), virtual_files.clone())))
            });

            let server = hyper::Server::try_bind(&addr);
            if let Err(bind_err) = server {
                error!("Couldn't bind {}: {}", bind_addr, bind_err);
                tx_from_sock.send(SocketMessage::Error(bind_err.to_string())).unwrap_or_default();
                return;
            }
            let server = server.unwrap().serve(make_service);
            let graceful = server.with_graceful_shutdown(async move {
                let virtual_files = virtual_files_two_the_clone_wars.clone();
                loop {
                    match rx_to_sock.recv().await {
                        Some(SocketMessage::Disconnect) | Some(SocketMessage::Error(_)) | None => {
                            break
                        },
                        Some(SocketMessage::FileAdd(filename, filedata)) => {
                            let mut vfiles = virtual_files.write().expect("Lock is poisoned");
                            vfiles.insert(filename, filedata);
                        },
                        Some(SocketMessage::FileRemove(filename)) => {
                            let mut vfiles = virtual_files.write().expect("Lock is poisoned");
                            vfiles.remove(&filename);
                        },
                        _ => {} // ignore sends?
                    }
                }
                info!("HTTP server trying to gracefully exit?");
            });
            info!("HTTP server running on http://{}/", addr);
            if let Err(err) = graceful.await {
                tx_from_sock.send(SocketMessage::Error(err.to_string())).unwrap_or_default(); // don't care at this point
            }
            info!("HTTP server stopped.");
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None,
            last_client_handle: 0
        });
        let new_handle = self._next_handle();
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    pub fn listen_ws(&mut self, addr: String) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            info!("WS server spawned");
            let listener = match TcpListener::bind(&addr).await {
                Ok(listener) => listener,
                Err(tcp_err) => {
                    tx_from_sock.send(SocketMessage::Error(tcp_err.to_string())).unwrap_or_default();
                    return;
                }
            };
            info!("WS server waiting for connections on {}", addr);
            tx_from_sock.send(SocketMessage::Connect).expect("oh boy");                    
            loop {
                tokio::select! {
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(SocketMessage::Message(_msg)) => {}, // server socket ignores sends
                            _ => break
                        }
                    },
                    new_client = listener.accept() => {
                        match new_client {
                            Ok((tcp_stream, addr)) => {
                                tokio::spawn(accept_ws(tcp_stream, addr, tx_from_sock.clone()));
                            },
                            Err(msg) => {
                                tx_from_sock.send(SocketMessage::Error(msg.to_string())).expect("TX error on socket error");
                                break;
                            }
                        }
                    },
                };
            }
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None,
            last_client_handle: 0
        });
        let new_handle = self._next_handle();
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    pub fn listen_tcp(&mut self, addr: String) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            info!("TCP server spawned");
            let listener = match TcpListener::bind(&addr).await {
                Ok(listener) => listener,
                Err(tcp_err) => {
                    tx_from_sock.send(SocketMessage::Error(tcp_err.to_string())).unwrap_or_default();
                    return;
                }
            };
            info!("TCP server waiting for connections on {}", addr);
            tx_from_sock.send(SocketMessage::Connect).expect("oh boy");                    
            loop {
                tokio::select! {
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(SocketMessage::Message(_msg)) => {}, // server socket ignores sends
                            _ => break
                        }
                    },
                    new_client = listener.accept() => {
                        match new_client {
                            Ok((tcp_stream, addr)) => {
                                tokio::spawn(accept_tcp(tcp_stream, addr, Some(tx_from_sock.clone())));
                            },
                            Err(msg) => {
                                tx_from_sock.send(SocketMessage::Error(msg.to_string())).expect("TX error on socket error");
                                break;
                            }
                        }
                    },
                };
            }
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None,
            last_client_handle: 0
        });
        let new_handle = self._next_handle();
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    pub fn open_ws(&mut self, url: String) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            info!("WS client spawned");
            let real_url = url::Url::parse(&url);
            if let Err(url_err) = real_url {
                error!("Invalid URL: {}", url);
                tx_from_sock.send(SocketMessage::Error(url_err.to_string())).unwrap_or_default();
                return;
            }

            info!("WS client attempting to connect to {}", url);
            match connect_async(real_url.unwrap()).await {
                Ok((mut ws_stream, _)) => {
                    tx_from_sock.send(SocketMessage::Connect).expect("oh boy");
                    loop {
                        tokio::select! {
                            from_c_message = rx_to_sock.recv() => {
                                match from_c_message {
                                    Some(SocketMessage::Message(msg)) => {
                                        ws_stream.send(tungstenite::protocol::Message::Text(msg)).await.expect("WS send error");
                                    },
                                    Some(SocketMessage::BinaryMessage(msg)) => {
                                        ws_stream.send(tungstenite::protocol::Message::Binary(msg)).await.expect("WS send error");
                                    },
                                    _ => break
                                }
                            },
                            from_sock_message = ws_stream.next() => {
                                match from_sock_message {
                                    Some(Ok(msg)) => {
                                        tx_from_sock.send(SocketMessage::BinaryMessage(msg.into_data())).expect("TX error on socket message");
                                    },
                                    Some(Err(msg)) => {
                                        tx_from_sock.send(SocketMessage::Error(msg.to_string())).expect("TX error on socket error");
                                        break;
                                    },
                                    None => {
                                        tx_from_sock.send(SocketMessage::Disconnect).expect("TX error on remote socket close");
                                        break;
                                    }
                                }
                            },
                        };
                    }
                    info!("Closing websocket!");
                    ws_stream.close(None).await.unwrap_or_default(); // if this errors we don't care
                },
                Err(err) => {
                    error!("WS client connection error: {}", err);
                    tx_from_sock.send(SocketMessage::Error(err.to_string())).expect("TX error on connection error");
                }
            }
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None,
            last_client_handle: 0
        });
        let new_handle = self._next_handle();
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    pub fn open_tcp(&mut self, addr: String) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            info!("TCP client attempting to connect to {}", addr);
            let mut buf = [0; 65536];
            match TcpStream::connect(addr).await {
                Ok(mut tcp_stream) => {
                    tx_from_sock.send(SocketMessage::Connect).expect("oh boy");
                    loop {
                        tokio::select! {
                            from_c_message = rx_to_sock.recv() => {
                                match from_c_message {
                                    Some(SocketMessage::Message(msg)) => {
                                        tcp_stream.write_all(msg.as_bytes()).await.expect("TCP send error");
                                    },
                                    Some(SocketMessage::BinaryMessage(msg)) => {
                                        tcp_stream.write_all(&msg).await.expect("TCP send error");
                                    },
                                    _ => break
                                }
                            },
                            _ = tcp_stream.readable() => {
                                match tcp_stream.try_read(&mut buf){
                                    Ok(n) => {
                                        // TODO: can we avoid these copies? Does it matter?
                                        let submessage = buf[0..n].to_vec();
                                        tx_from_sock.send(SocketMessage::BinaryMessage(submessage)).expect("TX error on socket message");
                                    }
                                    Err(ref e) if e.kind() == tokio::io::ErrorKind::WouldBlock => {
                                        // no effect?
                                    }
                                    Err(err) => {
                                        tx_from_sock.send(SocketMessage::Error(err.to_string())).expect("TX error on socket error");
                                        break;
                                    }
                                }
                            },
                        };
                    }
                    info!("Closing TCP socket!");
                    tcp_stream.shutdown().await.unwrap_or_default(); // if this errors we don't care
                },
                Err(err) => {
                    error!("TCP client connection error: {}", err);
                    tx_from_sock.send(SocketMessage::Error(err.to_string())).expect("TX error on connection error");
                }
            }
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None,
            last_client_handle: 0
        });
        let new_handle = self._next_handle();
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    async fn _handle_get(url: String, body_only: bool, dest: std::sync::mpsc::Sender<SocketMessage>) {
        info!("HTTP GET: {}", url);
        _handle_http_response(reqwest::get(&url).await, body_only, dest).await;
    }

    pub fn open_http_get_simple(&mut self, url: String, body_only: bool) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            let get_handler = PollnetContext::_handle_get(url, body_only, tx_from_sock);
            tokio::pin!(get_handler);
            loop {
                tokio::select! {
                    _ = &mut get_handler => break,
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(SocketMessage::Disconnect) => break,
                            _ => ()
                        }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None,
            last_client_handle: 0
        });
        let new_handle = self._next_handle();
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    async fn _handle_post(url: String, ret_body_only: bool, content_type: String, body: Vec<u8>, dest: std::sync::mpsc::Sender<SocketMessage>) {
        info!("HTTP POST: {} (w/ {})", url, content_type);
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send().await;
        _handle_http_response(resp, ret_body_only, dest).await;
    }

    pub fn open_http_post_simple(&mut self, url: String, ret_body_only: bool, content_type: String, body: Vec<u8>) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            let post_handler = PollnetContext::_handle_post(
                url, ret_body_only, content_type, body, tx_from_sock);
            tokio::pin!(post_handler);
            loop {
                tokio::select! {
                    _ = &mut post_handler => break,
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(SocketMessage::Disconnect) => break,
                            _ => ()
                        }
                    },
                }
            }
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None,
            last_client_handle: 0
        });
        let new_handle = self._next_handle();
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    pub fn close_all(&mut self) {
        info!("Closing all sockets!");
        for (_, sock) in self.sockets.iter_mut() {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    // don't care about errors at this point
                    block_on(sock.tx.send(SocketMessage::Disconnect)).unwrap_or_default();
                    sock.status = SocketStatus::CLOSED;
                },
                _ => (),
            }
        }
        self.sockets.clear(); // everything should be closed and safely droppable
    }

    pub fn close(&mut self, handle: u32) {
        if let Some(sock) = self.sockets.get_mut(&handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    match block_on(sock.tx.send(SocketMessage::Disconnect)) {
                        _ => ()
                    }
                    sock.status = SocketStatus::CLOSED;
                },
                _ => (),
            }
            // Note: since we don't wait here for any kind of "disconnect" reply,
            // a socket that has been closed should just return without sending a reply
            self.sockets.remove(&handle);
        }
    }

    pub fn send(&mut self, handle: u32, msg: String) {
        if let Some(sock) = self.sockets.get_mut(&handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::Message(msg)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn send_binary(&mut self, handle: u32, msg: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(&handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::BinaryMessage(msg)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn add_virtual_file(&mut self, handle: u32, filename: String, filedata: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(&handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::FileAdd(filename, filedata)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn remove_virtual_file(&mut self, handle: u32, filename: String) {
        if let Some(sock) = self.sockets.get_mut(&handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::FileRemove(filename)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn update(&mut self, handle: u32, blocking: bool) -> SocketResult {
        let sock = match self.sockets.get_mut(&handle) {
            Some(sock) => sock,
            None => return SocketResult::INVALIDHANDLE,
        };

        match sock.status {
            SocketStatus::OPEN | SocketStatus::OPENING => {
                // This block is apparently impossible to move into a helper function
                // for borrow checker "reasons"
                let result = if blocking {
                    sock.rx.recv().map_err(|_err| RecvError::Disconnected)
                } else {
                    sock.rx.try_recv().map_err(|err| match err {
                        std::sync::mpsc::TryRecvError::Empty => RecvError::Empty,
                        std::sync::mpsc::TryRecvError::Disconnected => RecvError::Disconnected,
                    })
                };

                match result {
                    Ok(SocketMessage::Connect) => {
                        sock.status = SocketStatus::OPEN;
                        SocketResult::OPENING
                    },
                    Ok(SocketMessage::Disconnect) | Err(RecvError::Disconnected) => {
                        sock.status = SocketStatus::CLOSED;
                        SocketResult::CLOSED
                    },
                    Ok(SocketMessage::Message(msg)) => {
                        sock.message = Some(msg.into_bytes());
                        SocketResult::HASDATA
                    },
                    Ok(SocketMessage::BinaryMessage(msg)) => {
                        sock.message = Some(msg);
                        SocketResult::HASDATA
                    },
                    Ok(SocketMessage::Error(err)) => {
                        sock.error = Some(err);
                        sock.status = SocketStatus::ERROR;
                        SocketResult::ERROR
                    },
                    Ok(SocketMessage::NewClient(conn)) => {
                        // can't use self._next_handle() either for questionable reasons
                        let new_handle = PollnetContext::_next_handle_that_satisfies_the_borrow_checker(&mut self.next_handle);
                        sock.last_client_handle = new_handle;
                        sock.message = Some(conn.id.into_bytes());
                        let client_socket = Box::new(PollnetSocket{
                            tx: conn.tx,
                            rx: conn.rx,
                            status: SocketStatus::OPEN, // assume client sockets start open?
                            message: None,
                            error: None,
                            last_client_handle: 0,
                        });
                        self.sockets.insert(new_handle, client_socket);
                        SocketResult::NEWCLIENT
                    },
                    Ok(_) => SocketResult::NODATA,
                    Err(RecvError::Empty) => SocketResult::NODATA,
                }
            },
            SocketStatus::CLOSED => SocketResult::CLOSED,
            _ => SocketResult::ERROR
        }
    }


    pub fn shutdown(&mut self) {
        info!("Starting shutdown");
        self.close_all();
        info!("All sockets should be closed?");
        if let Some(tx) = self.shutdown_tx.take() {
            tx.send(0).unwrap_or_default();
        }
        if let Some(handle) = self.thread.take() {
            handle.join().unwrap_or_default();
        }
        info!("Thread should be joined?");
    }
}