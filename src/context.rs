use futures::executor::block_on;
use futures_util::{future, SinkExt, StreamExt};
use log::{error, info, warn};
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
                SocketStatus::OPEN | SocketStatus::OPENING => {
                    // don't care about errors at this point
                    block_on(sock.tx.send(SocketMessage::Disconnect)).unwrap_or_default();
                    sock.status = SocketStatus::CLOSED;
                }
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
                        _ => (),
                    }
                    sock.status = SocketStatus::CLOSED;
                }
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
                SocketStatus::OPEN | SocketStatus::OPENING => sock
                    .tx
                    .try_send(SocketMessage::Message(msg))
                    .unwrap_or_default(),
                _ => (),
            };
        }
    }

    pub fn send_binary(&mut self, handle: SocketHandle, msg: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => sock
                    .tx
                    .try_send(SocketMessage::BinaryMessage(msg))
                    .unwrap_or_default(),
                _ => (),
            };
        }
    }

    pub fn add_virtual_file(&mut self, handle: SocketHandle, filename: String, filedata: Vec<u8>) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => sock
                    .tx
                    .try_send(SocketMessage::FileAdd(filename, filedata))
                    .unwrap_or_default(),
                _ => (),
            };
        }
    }

    pub fn remove_virtual_file(&mut self, handle: SocketHandle, filename: String) {
        if let Some(sock) = self.sockets.get_mut(handle) {
            match sock.status {
                SocketStatus::OPEN | SocketStatus::OPENING => sock
                    .tx
                    .try_send(SocketMessage::FileRemove(filename))
                    .unwrap_or_default(),
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
                    }
                    Ok(SocketMessage::Disconnect) | Err(RecvError::Disconnected) => {
                        sock.status = SocketStatus::CLOSED;
                        SocketResult::CLOSED
                    }
                    Ok(SocketMessage::Message(msg)) => {
                        sock.message = Some(msg.into_bytes());
                        SocketResult::HASDATA
                    }
                    Ok(SocketMessage::BinaryMessage(msg)) => {
                        sock.message = Some(msg);
                        SocketResult::HASDATA
                    }
                    Ok(SocketMessage::Error(err)) => {
                        sock.error = Some(err);
                        sock.status = SocketStatus::ERROR;
                        SocketResult::ERROR
                    }
                    Ok(SocketMessage::NewClient(conn)) => {
                        sock.message = Some(conn.id.into_bytes());
                        let client_socket = Box::new(PollnetSocket {
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
                    }
                    Ok(_) => SocketResult::NODATA,
                    Err(RecvError::Empty) => SocketResult::NODATA,
                }
            }
            SocketStatus::CLOSED => SocketResult::CLOSED,
            _ => SocketResult::ERROR,
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
