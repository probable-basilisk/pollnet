use std::net::SocketAddr;
use slotmap::{HopSlotMap, Key};
use log::{error, warn, info};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::{connect_async, accept_async};
use futures::executor::block_on;
use std::thread;
use futures_util::{SinkExt, StreamExt, future};

mod httpfuncs;

slotmap::new_key_type! {
  pub struct SocketHandle;
}

impl From<u64> for SocketHandle {
    fn from(item: u64) -> Self {
        Self::from(slotmap::KeyData::from_ffi(item))
    }
}
impl From<SocketHandle> for u64 {
    fn from(item: SocketHandle) -> Self {
        item.data().as_ffi()
    }
}

#[derive(Debug)]
pub enum RecvError {
    Empty,
    Disconnected,
}

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

#[derive(Copy, Clone)]
pub enum SocketStatus {
    INVALIDHANDLE,
    CLOSED,
    OPEN,
    OPENING,
    ERROR,
}

impl From<SocketResult> for u32 {
    fn from(item: SocketResult) -> Self {
        item as u32
    }
}
impl From<SocketStatus> for u32 {
    fn from(item: SocketStatus) -> Self {
        item as u32
    }
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
    pub last_client_handle: SocketHandle,
}

pub struct PollnetContext {
    pub sockets: HopSlotMap<SocketHandle, Box<PollnetSocket>>,
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
            rt_handle: handle_rx.recv().unwrap(),
            thread,
            shutdown_tx,
            sockets: HopSlotMap::with_key()
        }
    }


    pub fn listen_ws(&mut self, addr: String) -> SocketHandle {
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
            last_client_handle: SocketHandle::null()
        });
        self.sockets.insert(socket)
    }

    pub fn listen_tcp(&mut self, addr: String) -> SocketHandle {
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
            last_client_handle: SocketHandle::null()
        });
        self.sockets.insert(socket)
    }

    pub fn open_ws(&mut self, url: String) -> SocketHandle {
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
            last_client_handle: SocketHandle::null()
        });
        self.sockets.insert(socket)
    }

    pub fn open_tcp(&mut self, addr: String) -> SocketHandle {
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
            last_client_handle: SocketHandle::null()
        });
        self.sockets.insert(socket)
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

    pub fn close(&mut self, handle: SocketHandle) {
        if let Some(sock) = self.sockets.get_mut(handle) {
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
            self.sockets.remove(handle);
        }
    }

    pub fn send(&mut self, handle: SocketHandle, msg: String) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::Message(msg)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn send_binary(&mut self, handle: SocketHandle, msg: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::BinaryMessage(msg)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn add_virtual_file(&mut self, handle: SocketHandle, filename: String, filedata: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::FileAdd(filename, filedata)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn remove_virtual_file(&mut self, handle: SocketHandle, filename: String) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    sock.tx.try_send(SocketMessage::FileRemove(filename)).unwrap_or_default()
                },
                _ => (),
            };
        }
    }

    pub fn update(&mut self, handle: SocketHandle, blocking: bool) -> SocketResult {
        let sock = match self.sockets.get_mut(handle) {
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
                        sock.message = Some(conn.id.into_bytes());
                        let client_socket = Box::new(PollnetSocket{
                            tx: conn.tx,
                            rx: conn.rx,
                            status: SocketStatus::OPEN, // assume client sockets start open?
                            message: None,
                            error: None,
                            last_client_handle: SocketHandle::null(),
                        });
                        let newhandle = self.sockets.insert(client_socket);
                        // Note this horrible hack so it won't complain about multiple mutable borrows
                        // of self.sockets at the same time
                        let sock2 = match self.sockets.get_mut(handle) {
                            Some(sock) => sock,
                            None => return SocketResult::INVALIDHANDLE,
                        };
                        sock2.last_client_handle = newhandle;
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