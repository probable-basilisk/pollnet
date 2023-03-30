use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio_tungstenite::WebSocketStream;
use tungstenite::protocol::Message;

use super::*;

async fn websocket_poll_loop<S>(
    mut ws_stream: WebSocketStream<S>,
    tx_from_sock: std::sync::mpsc::Sender<PollnetMessage>,
    mut rx_to_sock: tokio::sync::mpsc::Receiver<PollnetMessage>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if tx_from_sock.send(PollnetMessage::Connect).is_err() {
        error!("Channel closed before WS finished opening.");
        return;
    }
    loop {
        tokio::select! {
            from_c_message = rx_to_sock.recv() => {
                match from_c_message {
                    Some(PollnetMessage::Text(msg)) => {
                        if let Err(e) = ws_stream.send(Message::Text(msg)).await {
                            debug!("WS send error.");
                            tx_from_sock.send(PollnetMessage::Error(e.to_string())).unwrap_or_default();
                            break;
                        };
                    },
                    Some(PollnetMessage::Binary(msg)) => {
                        if let Err(e) = ws_stream.send(Message::Binary(msg)).await {
                            debug!("WS send error.");
                            tx_from_sock.send(PollnetMessage::Error(e.to_string())).unwrap_or_default();
                            break;
                        };
                    },
                    Some(PollnetMessage::Disconnect) => {
                        debug!("Client-side disconnect.");
                        break
                    },
                    None => {
                        warn!("Channel closed w/o disconnect message.");
                        break
                    },
                    _ => {
                        error!("Invalid message to WS!");
                    }
                }
            },
            from_sock_message = ws_stream.next() => {
                match from_sock_message {
                    Some(Ok(msg)) => {
                        if tx_from_sock.send(PollnetMessage::Binary(msg.into_data())).is_err() {
                            error!("Channel closed in some bad way.");
                            break;
                        }
                    },
                    Some(Err(msg)) => {
                        debug!("Socket error.");
                        tx_from_sock.send(PollnetMessage::Error(msg.to_string())).unwrap_or_default();
                        break;
                    },
                    None => {
                        debug!("Socket disconnect.");
                        tx_from_sock.send(PollnetMessage::Disconnect).unwrap_or_default();
                        break;
                    }
                }
            },
        };
    }
    info!("Closing websocket!");
    // if this errors we don't care
    ws_stream.close(None).await.unwrap_or_default();
}

async fn accept_ws<S>(
    stream: S,
    addr: SocketAddr,
    outer_tx: std::sync::mpsc::Sender<PollnetMessage>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    //rx_to_sock: tokio::sync::mpsc::Receiver<SocketMessage>, tx_from_sock: std::sync::mpsc::Sender<SocketMessage>) {
    let (tx_to_sock, rx_to_sock) = tokio::sync::mpsc::channel(100);
    let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

    outer_tx
        .send(PollnetMessage::NewClient(ClientConn {
            tx: tx_to_sock,
            rx: rx_from_sock,
            id: addr.to_string(), //"BLURGH".to_string(),
        }))
        .expect("this shouldn't ever break?");

    match accept_async(stream).await {
        Ok(ws_stream) => {
            websocket_poll_loop(ws_stream, tx_from_sock, rx_to_sock).await;
        }
        Err(err) => {
            error!("connection error: {}", err);
            tx_from_sock
                .send(PollnetMessage::Error(err.to_string()))
                .expect("TX error on connection error");
        }
    }
}

impl PollnetContext {
    pub fn open_ws(&mut self, url: String) -> SocketHandle {
        let (tx_to_sock, rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            info!("WS client spawned");
            let real_url = match url::Url::parse(&url) {
                Ok(v) => v,
                Err(url_err) => {
                    error!("Invalid URL: {}", url);
                    tx_from_sock
                        .send(PollnetMessage::Error(url_err.to_string()))
                        .unwrap_or_default();
                    return;
                }
            };

            info!("WS client attempting to connect to {}", url);
            match connect_async(real_url).await {
                Ok((ws_stream, _)) => {
                    websocket_poll_loop(ws_stream, tx_from_sock, rx_to_sock).await;
                }
                Err(err) => {
                    error!("WS client connection error: {}", err);
                    tx_from_sock
                        .send(PollnetMessage::Error(err.to_string()))
                        .expect("TX error on connection error");
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

    pub fn listen_ws(&mut self, addr: String) -> SocketHandle {
        let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            info!("WS server spawned");
            let listener = match TcpListener::bind(&addr).await {
                Ok(listener) => listener,
                Err(tcp_err) => {
                    tx_from_sock.send(PollnetMessage::Error(tcp_err.to_string())).unwrap_or_default();
                    return;
                }
            };
            info!("WS server waiting for connections on {}", addr);
            tx_from_sock.send(PollnetMessage::Connect).expect("oh boy");                    
            loop {
                tokio::select! {
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(PollnetMessage::Text(_msg)) => {}, // server socket ignores sends
                            _ => break
                        }
                    },
                    new_client = listener.accept() => {
                        match new_client {
                            Ok((tcp_stream, addr)) => {
                                tokio::spawn(accept_ws(tcp_stream, addr, tx_from_sock.clone()));
                            },
                            Err(msg) => {
                                tx_from_sock.send(PollnetMessage::Error(msg.to_string())).expect("TX error on socket error");
                                break;
                            }
                        }
                    },
                };
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
