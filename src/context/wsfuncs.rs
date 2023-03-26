use super::*;

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

impl PollnetContext {
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
}