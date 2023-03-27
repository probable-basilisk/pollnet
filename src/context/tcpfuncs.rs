use super::*;

async fn accept_tcp(
    mut tcp_stream: TcpStream,
    addr: SocketAddr,
    outer_tx: Option<std::sync::mpsc::Sender<SocketMessage>>,
) {
    let (tx_to_sock, mut rx_to_sock) = tokio::sync::mpsc::channel(100);
    let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

    if let Some(tx) = outer_tx {
        tx.send(SocketMessage::NewClient(ClientConn {
            tx: tx_to_sock,
            rx: rx_from_sock,
            id: addr.to_string(),
        }))
        .expect("this shouldn't ever break?");
    }
           
    tx_from_sock.send(SocketMessage::Connect).expect("oh boy");
    // TODO: should this be bigger? Or automatically grow or something?
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
                    Ok(0) => {
                        // Reading zero bytes indicates that the stream has closed
                        tx_from_sock.send(SocketMessage::Disconnect).expect("TX error on disconnect");
                        break;
                    },
                    Ok(n) => {
                        // TODO: can we avoid these copies? Does it matter?
                        info!("Read {:} bytes!", n);
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
