extern crate url;

use std::collections::HashMap;
use std::thread;
use tokio::runtime; // 0.1.15
use tokio_tungstenite::{connect_async};
use futures::executor::block_on;
use futures_util::{SinkExt, StreamExt};
use std::os::raw::c_char;
use std::ffi::CStr;

#[repr(C)]
#[derive(Copy, Clone)]
pub enum SocketResult {
    INVALIDHANDLE,
    CLOSED,
    OPENING,
    NODATA,
    HASDATA,
    ERROR,
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


enum SocketMessage {
    Connect,
    Disconnect,
    Message(String),
    Error(String)
}


pub struct PollnetSocket {
    status: SocketStatus,
    tx: tokio::sync::mpsc::Sender<SocketMessage>,
    rx: std::sync::mpsc::Receiver<SocketMessage>,
    message: Option<String>,
    error: Option<String>
}


impl PollnetSocket {
    fn update(&mut self) -> SocketResult {
        match self.rx.try_recv() {
            Ok(SocketMessage::Connect) => {
                self.status = SocketStatus::OPEN;
                SocketResult::OPENING
            },
            Ok(SocketMessage::Disconnect) => {
                self.status = SocketStatus::CLOSED;
                SocketResult::CLOSED
            },
            Ok(SocketMessage::Message(msg)) => {
                self.message = Some(msg);
                SocketResult::HASDATA
            },
            Ok(SocketMessage::Error(err)) => {
                self.error = Some(err);
                self.status = SocketStatus::ERROR;
                SocketResult::ERROR
            },
            _ => {
                SocketResult::NODATA
            }
        }
    }

    fn send(&mut self, msg: String) {
        match self.status {
            SocketStatus::OPEN => self.tx.try_send(SocketMessage::Message(msg)).unwrap_or_default(),
            _ => (),
        };
    }

    fn close(&mut self) {
        match self.status {
            SocketStatus::OPEN | SocketStatus::OPENING => {
                match block_on(self.tx.send(SocketMessage::Disconnect)) {
                    _ => ()
                }
                self.status = SocketStatus::CLOSED;
            },
            _ => (),
        }
    }
}

pub struct PollnetContext {
    sockets: HashMap<u32, Box<PollnetSocket>>,
    next_handle: u32,
    thread: Option<thread::JoinHandle<()>>,
    rt_handle: tokio::runtime::Handle,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<i32>>,
}

impl PollnetContext {
    fn new() -> PollnetContext {
        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let shutdown_tx = Some(shutdown_tx);

        let thread = Some(thread::spawn(move || {
            let mut rt = runtime::Builder::new()
                    .basic_scheduler()
                    .build()
                    .expect("Unable to create the runtime");

            // Send handle back out so we can store it?
            handle_tx
                .send(rt.handle().clone())
                .expect("Unable to give runtime handle to another thread");

            // Continue running until notified to shutdown
            rt.block_on(async {
                shutdown_rx.await.unwrap();
            });

            //eprintln!("Runtime finished");
        }));

        PollnetContext{
            next_handle: 0,
            rt_handle: handle_rx.recv().unwrap(),
            thread: thread,
            shutdown_tx: shutdown_tx,
            sockets: HashMap::new()
        }
    }

    fn open_ws(&mut self, url: String) -> u32 {
        let new_handle = self.next_handle;
        self.next_handle += 1;

        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        // Spawn a future onto the runtime
        self.rt_handle.spawn(async move {
            println!("now running on a worker thread");
            let real_url = url::Url::parse(&url).unwrap();

            match connect_async(real_url).await {
                Ok((mut ws_stream, _)) => {
                    tx_from_sock.send(SocketMessage::Connect).expect("oh boy");
                    loop {
                        tokio::select! {
                            from_c_message = rx_to_sock.recv() => {
                                match from_c_message {
                                    Some(SocketMessage::Message(msg)) => {
                                        ws_stream.send(tungstenite::protocol::Message::Text(msg)).await.expect("???");
                                    }
                                    _ => break
                                }
                            },
                            from_sock_message = ws_stream.next() => {
                                if let Some(Ok(msg)) = from_sock_message {
                                    tx_from_sock.send(SocketMessage::Message(msg.to_string())).expect("????");
                                } else {
                                    break;
                                }
                            },
                        };
                    }
                    tx_from_sock.send(SocketMessage::Disconnect).expect("?????");
                },
                Err(err) => {
                    tx_from_sock.send(SocketMessage::Error(err.to_string())).expect("??????");
                }
            }
        });

        let socket = Box::new(PollnetSocket{
            tx: tx_to_sock,
            rx: rx_from_sock,
            status: SocketStatus::OPENING,
            message: None,
            error: None
        });
        self.sockets.insert(new_handle, socket);

        new_handle
    }

    fn close(&mut self, handle: u32) {
        if let Some(sock) = self.sockets.get_mut(&handle) {
            //block_on(sock.tx.send(SocketMessage::Disconnect)).unwrap_or_default();
            sock.close();
            self.sockets.remove(&handle);
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
    if let Some(socket) = ctx.sockets.get_mut(&handle) {
        socket.send(msg)
    }
}

#[no_mangle]
pub extern fn pollnet_update(ctx: *mut PollnetContext, handle: u32) -> SocketResult {
    let ctx = unsafe{&mut *ctx};
    if let Some(socket) = ctx.sockets.get_mut(&handle) {
        socket.update()
    } else {
        SocketResult::INVALIDHANDLE
    }
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