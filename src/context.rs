use futures::executor::block_on;
use futures_util::{future, SinkExt, StreamExt};
use log::{debug, error, info, warn};
use slotmap::{HopSlotMap, Key};
use std::net::SocketAddr;
use std::thread;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime;
use tokio_tungstenite::{accept_async, connect_async};

mod httpfuncs;
mod tcpfuncs;
mod wsfuncs;

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
    InvalidHandle,
    Closed,
    Opening,
    NoData,
    HasData,
    Error,
    NewClient,
}

#[derive(Copy, Clone)]
pub enum SocketStatus {
    InvalidHandle,
    Closed,
    Open,
    Opening,
    Error,
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
    tx: tokio::sync::mpsc::Sender<PollnetMessage>,
    rx: std::sync::mpsc::Receiver<PollnetMessage>,
    id: String,
}

enum PollnetMessage {
    Connect,
    Disconnect,
    Text(String),
    Binary(Vec<u8>),
    Error(String),
    NewClient(ClientConn),
    FileAdd(String, Vec<u8>),
    FileRemove(String),
}

pub struct PollnetSocket {
    pub status: SocketStatus,
    tx: tokio::sync::mpsc::Sender<PollnetMessage>,
    rx: std::sync::mpsc::Receiver<PollnetMessage>,
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

impl PollnetContext {
    pub fn new() -> PollnetContext {
        if let Err(err) = env_logger::try_init() {
            warn!("Multiple contexts created!: {}", err)
        };

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

        PollnetContext {
            rt_handle: handle_rx.recv().unwrap(),
            thread,
            shutdown_tx,
            sockets: HopSlotMap::with_key(),
        }
    }

    pub fn close_all(&mut self) {
        info!("Closing all sockets!");
        for (_, sock) in self.sockets.iter_mut() {
            match sock.status {
                SocketStatus::Open | SocketStatus::Opening => {
                    // don't care about errors at this point
                    block_on(sock.tx.send(PollnetMessage::Disconnect)).unwrap_or_default();
                    sock.status = SocketStatus::Closed;
                }
                _ => (),
            }
        }
        self.sockets.clear(); // everything should be closed and safely droppable
    }

    pub fn close(&mut self, handle: SocketHandle) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::Open | SocketStatus::Opening => {
                    if let Err(e) = block_on(sock.tx.send(PollnetMessage::Disconnect)) {
                        warn!("Socket already closed from other end: {:}", e);
                    }
                    sock.status = SocketStatus::Closed;
                }
                _ => (),
            };
            debug!("Removing handle: {:?}", handle);
            // Note: since we don't wait here for any kind of "disconnect" reply,
            // a socket that has been closed should just return without sending a reply
            self.sockets.remove(handle);
        }
    }

    pub fn send(&mut self, handle: SocketHandle, msg: String) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::Open | SocketStatus::Opening => sock
                    .tx
                    .try_send(PollnetMessage::Text(msg))
                    .unwrap_or_default(),
                _ => (),
            };
        }
    }

    pub fn send_binary(&mut self, handle: SocketHandle, msg: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::Open | SocketStatus::Opening => sock
                    .tx
                    .try_send(PollnetMessage::Binary(msg))
                    .unwrap_or_default(),
                _ => (),
            };
        }
    }

    pub fn add_virtual_file(&mut self, handle: SocketHandle, filename: String, filedata: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::Open | SocketStatus::Opening => sock
                    .tx
                    .try_send(PollnetMessage::FileAdd(filename, filedata))
                    .unwrap_or_default(),
                _ => (),
            };
        }
    }

    pub fn remove_virtual_file(&mut self, handle: SocketHandle, filename: String) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::Open | SocketStatus::Opening => sock
                    .tx
                    .try_send(PollnetMessage::FileRemove(filename))
                    .unwrap_or_default(),
                _ => (),
            };
        }
    }

    pub fn update(&mut self, handle: SocketHandle, blocking: bool) -> SocketResult {
        let sock = match self.sockets.get_mut(handle) {
            Some(sock) => sock,
            None => return SocketResult::InvalidHandle,
        };

        match sock.status {
            SocketStatus::Open | SocketStatus::Opening => {
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
                    Ok(PollnetMessage::Connect) => {
                        sock.status = SocketStatus::Open;
                        SocketResult::Opening
                    }
                    Ok(PollnetMessage::Disconnect) | Err(RecvError::Disconnected) => {
                        debug!("Socket disconnected.");
                        sock.status = SocketStatus::Closed;
                        SocketResult::Closed
                    }
                    Ok(PollnetMessage::Text(msg)) => {
                        debug!("Socket text message {:} bytes", msg.len());
                        sock.message = Some(msg.into_bytes());
                        SocketResult::HasData
                    }
                    Ok(PollnetMessage::Binary(msg)) => {
                        debug!("Socket binary message {:} bytes", msg.len());
                        sock.message = Some(msg);
                        SocketResult::HasData
                    }
                    Ok(PollnetMessage::Error(err)) => {
                        error!("Socket error: {:}", err);
                        sock.error = Some(err);
                        sock.status = SocketStatus::Error;
                        SocketResult::Error
                    }
                    Ok(PollnetMessage::NewClient(conn)) => {
                        sock.message = Some(conn.id.into_bytes());
                        let client_socket = Box::new(PollnetSocket {
                            tx: conn.tx,
                            rx: conn.rx,
                            status: SocketStatus::Open, // assume client sockets start open?
                            message: None,
                            error: None,
                            last_client_handle: SocketHandle::null(),
                        });
                        let newhandle = self.sockets.insert(client_socket);
                        // Note this horrible hack so it won't complain about multiple mutable borrows
                        // of self.sockets at the same time
                        let sock2 = match self.sockets.get_mut(handle) {
                            Some(sock) => sock,
                            None => return SocketResult::InvalidHandle,
                        };
                        sock2.last_client_handle = newhandle;
                        SocketResult::NewClient
                    }
                    Ok(_) => SocketResult::NoData,
                    Err(RecvError::Empty) => SocketResult::NoData,
                }
            }
            SocketStatus::Closed => SocketResult::Closed,
            _ => SocketResult::Error,
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

impl Default for PollnetContext {
    fn default() -> Self {
        Self::new()
    }
}
