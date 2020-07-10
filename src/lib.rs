extern crate url;

use std::collections::HashMap;
use std::thread;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
//use tokio::net::tcp::stream; // hmm why is this private????
use tokio::runtime; // 0.1.15
use tokio_tungstenite::{connect_async, accept_async};
use futures::executor::block_on;
use futures_util::{SinkExt, StreamExt, future};
use std::os::raw::c_char;
use std::ffi::CStr;

// use http::response::Builder as ResponseBuilder;
// use http::{header, StatusCode};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response};
use hyper_staticfile::Static;
use std::io::Error as IoError;
use std::path::Path;

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
    Error(String),
    NewClient(ClientConn),
}


pub struct PollnetSocket {
    status: SocketStatus,
    tx: tokio::sync::mpsc::Sender<SocketMessage>,
    rx: std::sync::mpsc::Receiver<SocketMessage>,
    message: Option<String>,
    error: Option<String>,
    last_client_handle: u32,
}

pub struct PollnetContext {
    sockets: HashMap<u32, Box<PollnetSocket>>,
    next_handle: u32,
    thread: Option<thread::JoinHandle<()>>,
    rt_handle: tokio::runtime::Handle,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<i32>>,
}

async fn accept_stream(tcp_stream: TcpStream, addr: SocketAddr, outer_tx: std::sync::mpsc::Sender<SocketMessage>) {//rx_to_sock: tokio::sync::mpsc::Receiver<SocketMessage>, tx_from_sock: std::sync::mpsc::Sender<SocketMessage>) {
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
                            }
                            _ => break
                        }
                    },
                    from_sock_message = ws_stream.next() => {
                        match from_sock_message {
                            Some(Ok(msg)) => {
                                tx_from_sock.send(SocketMessage::Message(msg.to_string())).expect("TX error on socket message");
                            },
                            Some(Err(msg)) => {
                                tx_from_sock.send(SocketMessage::Error(msg.to_string())).expect("TX error on socket error");
                                break;
                            },
                            None => {
                                break;
                            }
                        }
                    },
                };
            }
            tx_from_sock.send(SocketMessage::Disconnect).expect("TX error on disconnect");
        },
        Err(err) => {
            println!("Pollnet: connection error: {}", err);
            tx_from_sock.send(SocketMessage::Error(err.to_string())).expect("TX error on connection error");
        }
    }
}

async fn handle_http_request<B>(req: Request<B>, static_: Static) -> Result<Response<Body>, IoError> {
    static_.clone().serve(req).await
}

impl PollnetContext {
    fn new() -> PollnetContext {
        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let shutdown_tx = Some(shutdown_tx);

        let thread = Some(thread::spawn(move || {
            let mut rt = runtime::Builder::new()
                    .basic_scheduler()
                    .enable_all()
                    .build()
                    .expect("Unable to create the runtime");

            // Send handle back out so we can store it?
            handle_tx
                .send(rt.handle().clone())
                .expect("Unable to give runtime handle to another thread");

            // Continue running until notified to shutdown
            println!("Pollnet: tokio runtime starting");
            rt.block_on(async {
                shutdown_rx.await.unwrap();
            });
            println!("Pollnet: runtime shutdown");
        }));

        PollnetContext{
            next_handle: 1,
            rt_handle: handle_rx.recv().unwrap(),
            thread: thread,
            shutdown_tx: shutdown_tx,
            sockets: HashMap::new()
        }
    }

    fn _next_handle(&mut self) -> u32 {
        let new_handle = self.next_handle;
        self.next_handle += 1;
        new_handle
    }

    fn serve_static_http(&mut self, bind_addr: String, serve_dir: String) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            println!("Pollnet: HTTP server task spawned");
            let addr = bind_addr.parse();
            if let Err(_) = addr {
                tx_from_sock.send(SocketMessage::Error("Invalid TCP address".to_string())).unwrap_or_default();
                return;
            }
            let addr = addr.unwrap();

            let static_ = Static::new(Path::new(&serve_dir));

            let make_service = make_service_fn(|_| {
                let static_ = static_.clone();
                future::ok::<_, hyper::Error>(service_fn(move |req| handle_http_request(req, static_.clone())))
            });

            let server = hyper::Server::bind(&addr).serve(make_service);
            let graceful = server.with_graceful_shutdown(async move {
                loop {
                    match rx_to_sock.recv().await {
                        Some(SocketMessage::Disconnect) | Some(SocketMessage::Error(_)) | None => {
                            break
                        }
                        _ => {} // ignore sends?
                    }
                }
                println!("Server trying to gracefully exit?");
            });
            println!("Static server running on http://{}/, serving {}", addr, serve_dir);
            
            graceful.await.expect("Server failed");

            tx_from_sock.send(SocketMessage::Disconnect).expect("TX error on disconnect");
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

    fn listen_ws(&mut self, addr: String) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            println!("Pollnet: WS server task spawned");
            //let real_url = url::Url::parse(&url);
            let listener = TcpListener::bind(&addr).await;
            if let Err(tcp_err) = listener {
                tx_from_sock.send(SocketMessage::Error(tcp_err.to_string())).unwrap_or_default();
                return;
            }
            let mut listener = listener.unwrap();
            println!("Pollnet: waiting for connections");
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
                                tokio::spawn(accept_stream(tcp_stream, addr, tx_from_sock.clone()));
                            },
                            Err(msg) => {
                                tx_from_sock.send(SocketMessage::Error(msg.to_string())).expect("TX error on socket error");
                                break;
                            }
                        }
                    },
                };
            }
            tx_from_sock.send(SocketMessage::Disconnect).expect("TX error on disconnect");
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

    fn open_ws(&mut self, url: String) -> u32 {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            println!("Pollnet: WS task spawned");
            let real_url = url::Url::parse(&url);
            if let Err(url_err) = real_url {
                tx_from_sock.send(SocketMessage::Error(url_err.to_string())).unwrap_or_default();
                return;
            }

            println!("Pollnet: attempting to connect");
            match connect_async(real_url.unwrap()).await {
                Ok((mut ws_stream, _)) => {
                    tx_from_sock.send(SocketMessage::Connect).expect("oh boy");
                    loop {
                        tokio::select! {
                            from_c_message = rx_to_sock.recv() => {
                                match from_c_message {
                                    Some(SocketMessage::Message(msg)) => {
                                        ws_stream.send(tungstenite::protocol::Message::Text(msg)).await.expect("WS send error");
                                    }
                                    _ => break
                                }
                            },
                            from_sock_message = ws_stream.next() => {
                                match from_sock_message {
                                    Some(Ok(msg)) => {
                                        tx_from_sock.send(SocketMessage::Message(msg.to_string())).expect("TX error on socket message");
                                    },
                                    Some(Err(msg)) => {
                                        tx_from_sock.send(SocketMessage::Error(msg.to_string())).expect("TX error on socket error");
                                        break;
                                    },
                                    None => {
                                        break;
                                    }
                                }
                            },
                        };
                    }
                    tx_from_sock.send(SocketMessage::Disconnect).expect("TX error on disconnect");
                },
                Err(err) => {
                    println!("Pollnet: connection error: {}", err);
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

    fn close(&mut self, handle: u32) {
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
            self.sockets.remove(&handle);
        }
    }

    fn send(&mut self, handle: u32, msg: String) {
        if let Some(sock) = self.sockets.get_mut(&handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::Message(msg)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    fn update(&mut self, handle: u32) -> SocketResult {
        let sock = self.sockets.get_mut(&handle).unwrap();

        match sock.status {
            SocketStatus::OPEN | SocketStatus::OPENING => {
                // This block is apparently impossible to move into a helper function
                // for borrow checker "reasons"
                match sock.rx.try_recv() {
                    Ok(SocketMessage::Connect) => {
                        sock.status = SocketStatus::OPEN;
                        SocketResult::OPENING
                    },
                    Ok(SocketMessage::Disconnect) => {
                        sock.status = SocketStatus::CLOSED;
                        SocketResult::CLOSED
                    },
                    Ok(SocketMessage::Message(msg)) => {
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
                        let new_handle = self.next_handle;
                        self.next_handle += 1;
                        sock.last_client_handle = new_handle;
                        sock.message = Some(conn.id);
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
                    _ => {
                        SocketResult::NODATA
                    }
                }
            },
            SocketStatus::CLOSED => SocketResult::CLOSED,
            _ => SocketResult::ERROR
        }
    }


    fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            tx.send(0).unwrap_or_default();
        }
        if let Some(handle) = self.thread.take() {
            handle.join().unwrap_or_default();
        }
    }
}

#[no_mangle]
pub extern fn pollnet_init() -> *mut PollnetContext {
    Box::into_raw(Box::new(PollnetContext::new()))
}


#[no_mangle]
pub extern fn pollnet_shutdown(ctx: *mut PollnetContext) {
    let ctx = unsafe{&mut *ctx};
    ctx.shutdown();

    // take ownership and drop
    let b = unsafe{ Box::from_raw(ctx) };
    drop(b);
}


#[no_mangle]
pub extern fn pollnet_open_ws(ctx: *mut PollnetContext, url: *const c_char) -> u32 {
    let url = unsafe { CStr::from_ptr(url).to_string_lossy().into_owned() };
    let ctx = unsafe{&mut *ctx};
    ctx.open_ws(url)
}

#[no_mangle]
pub extern fn pollnet_listen_ws(ctx: *mut PollnetContext, addr: *const c_char) -> u32 {
    let addr = unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() };
    let ctx = unsafe{&mut *ctx};
    ctx.listen_ws(addr)
}


#[no_mangle]
pub extern fn pollnet_serve_static_http(ctx: *mut PollnetContext, addr: *const c_char, serve_dir: *const c_char) -> u32 {
    let addr = unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() };
    let serve_dir = unsafe { CStr::from_ptr(serve_dir).to_string_lossy().into_owned() };
    let ctx = unsafe{&mut *ctx};
    ctx.serve_static_http(addr, serve_dir)
}

#[no_mangle]
pub extern fn pollnet_close(ctx: *mut PollnetContext, handle: u32) {
    let ctx = unsafe{&mut *ctx};
    ctx.close(handle)
}

#[no_mangle]
pub extern fn pollnet_status(ctx: *mut PollnetContext, handle: u32) -> SocketStatus {
    let ctx = unsafe{&*ctx};
    if let Some(socket) = ctx.sockets.get(&handle) {
        socket.status
    } else {
        SocketStatus::INVALIDHANDLE
    }
}

#[no_mangle]
pub extern fn pollnet_send(ctx: *mut PollnetContext, handle: u32, msg: *const c_char) {
    let ctx = unsafe{&mut *ctx};
    let msg = unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() };
    ctx.send(handle, msg)
}

#[no_mangle]
pub extern fn pollnet_update(ctx: *mut PollnetContext, handle: u32) -> SocketResult {
    let ctx = unsafe{&mut *ctx};
    ctx.update(handle)
}

#[no_mangle]
pub extern fn pollnet_get(ctx: *mut PollnetContext, handle: u32, dest: *mut u8, dest_size: u32) -> i32 {
    let ctx = unsafe{&mut *ctx};
    if let Some(socket) = ctx.sockets.get_mut(&handle) {
        match socket.message.take() {
            Some(msg) => {
                let ncopy = msg.len();
                if ncopy < (dest_size as usize) {
                    unsafe {
                        std::ptr::copy_nonoverlapping(msg.as_ptr(), dest, ncopy);
                    }
                    ncopy as i32
                } else {
                    0
                }
            },
            None => 0,
        }
    } else {
        -1
    }
}

#[no_mangle]
pub extern fn pollnet_get_connected_client_handle(ctx: *mut PollnetContext, handle: u32) -> u32 {
    let ctx = unsafe{&mut *ctx};
    if let Some(socket) = ctx.sockets.get_mut(&handle) {
        socket.last_client_handle
    } else {
        0
    }
}

#[no_mangle]
pub extern fn pollnet_get_error(ctx: *mut PollnetContext, handle: u32, dest: *mut u8, dest_size: u32) -> i32 {
    let ctx = unsafe{&mut *ctx};
    if let Some(socket) = ctx.sockets.get_mut(&handle) {
        match socket.error.take() {
            Some(msg) => {
                let ncopy = msg.len();
                if ncopy < (dest_size as usize) {
                    unsafe {
                        std::ptr::copy_nonoverlapping(msg.as_ptr(), dest, ncopy);
                    }
                    ncopy as i32
                } else {
                    0
                }
            },
            None => 0,
        }
    } else {
        -1
    }
}