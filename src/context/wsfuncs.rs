use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio_tungstenite::WebSocketStream;
use tungstenite::protocol::Message;

use super::*;
use anyhow::anyhow;

// SendErrors are ironically not themselves sync so this
// function just turns any send error into an anyhow string error
fn eh<T>(r: Result<(), std::sync::mpsc::SendError<T>>) -> anyhow::Result<()> {
    r.map_err(|_| anyhow!("Channel TX Error"))
}

async fn websocket_poll_loop_inner<S>(
    ws_stream: &mut WebSocketStream<S>,
    tx_from_sock: std::sync::mpsc::Sender<PollnetMessage>,
    mut rx_to_sock: tokio::sync::mpsc::Receiver<PollnetMessage>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    eh(tx_from_sock.send(PollnetMessage::Connect))?;
    loop {
        tokio::select! {
            from_c_message = rx_to_sock.recv() => {
                match from_c_message {
                    Some(PollnetMessage::Text(msg)) => {
                        if let Err(e) = ws_stream.send(Message::Text(msg)).await {
                            debug!("WS send error.");
                            eh(tx_from_sock.send(PollnetMessage::Error(e.to_string())))?;
                            return Ok(());
                        };
                    },
                    Some(PollnetMessage::Binary(msg)) => {
                        if let Err(e) = ws_stream.send(Message::Binary(msg)).await {
                            debug!("WS send error.");
                            eh(tx_from_sock.send(PollnetMessage::Error(e.to_string())))?;
                            return Ok(());
                        };
                    },
                    Some(PollnetMessage::Disconnect) => {
                        debug!("Client-side disconnect.");
                        return Ok(());
                    },
                    None => {
                        warn!("Channel closed w/o disconnect message.");
                        return Ok(());
                    },
                    _ => {
                        error!("Invalid message to WS!");
                    }
                }
            },
            from_sock_message = ws_stream.next() => {
                match from_sock_message {
                    Some(Ok(msg)) => {
                        eh(tx_from_sock.send(PollnetMessage::Binary(msg.into_data())))?;
                    },
                    Some(Err(msg)) => {
                        info!("WS error.");
                        eh(tx_from_sock.send(PollnetMessage::Error(msg.to_string())))?;
                        return Ok(());
                    },
                    None => {
                        info!("WS disconnect.");
                        eh(tx_from_sock.send(PollnetMessage::Disconnect))?;
                        return Ok(());
                    }
                }
            },
        };
    }
}

async fn websocket_poll_loop<S>(
    mut ws_stream: WebSocketStream<S>,
    tx_from_sock: std::sync::mpsc::Sender<PollnetMessage>,
    rx_to_sock: tokio::sync::mpsc::Receiver<PollnetMessage>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if let Err(e) = websocket_poll_loop_inner(&mut ws_stream, tx_from_sock, rx_to_sock).await {
        error!("Unexpected WS loop termination: {:?}", e);
    }
    info!("Closing websocket!");
    // At this point errors don't matter
    ws_stream.close(None).await.unwrap_or_default();
}

async fn accept_ws_inner<S>(
    mut stream: S,
    addr: SocketAddr,
    outer_tx: std::sync::mpsc::Sender<PollnetMessage>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // rx_to_sock: tokio::sync::mpsc::Receiver<SocketMessage>
    // tx_from_sock: std::sync::mpsc::Sender<SocketMessage>
    let (tx_to_sock, rx_to_sock) = tokio::sync::mpsc::channel(100);
    let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

    let sendres = outer_tx.send(PollnetMessage::NewClient(ClientConn {
        tx: tx_to_sock,
        rx: rx_from_sock,
        id: addr.to_string(),
    }));
    if sendres.is_err() {
        stream.shutdown().await.unwrap_or_default();
        return Err(anyhow!("Fuck"));
    }

    match accept_async(stream).await {
        Ok(ws_stream) => {
            websocket_poll_loop(ws_stream, tx_from_sock, rx_to_sock).await;
        }
        Err(err) => {
            error!("connection error: {}", err);
            eh(tx_from_sock.send(PollnetMessage::Error(err.to_string())))?;
        }
    }
    Ok(())
}

async fn accept_ws<S>(
    stream: S,
    addr: SocketAddr,
    outer_tx: std::sync::mpsc::Sender<PollnetMessage>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if let Err(e) = accept_ws_inner(stream, addr, outer_tx).await {
        error!("Channel issue: {:?}", e);
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
                        .unwrap_or_default();
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
            if tx_from_sock.send(PollnetMessage::Connect).is_err() {
                error!("Channel died before entering serve loop!");
                return;
            }
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
                                tx_from_sock.send(PollnetMessage::Error(msg.to_string())).unwrap_or_default();
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
            data: None,
            last_client_handle: SocketHandle::null(),
        });
        self.sockets.insert(socket)
    }
}
