use super::*;

async fn tcp_poll_loop(
    tcp_stream: &mut TcpStream,
    tx_from_sock: std::sync::mpsc::Sender<PollnetMessage>,
    mut rx_to_sock: tokio::sync::mpsc::Receiver<PollnetMessage>,
) -> anyhow::Result<()> {
    check_tx(tx_from_sock.send(PollnetMessage::Connect))?;
    // TODO: should this be bigger? Or automatically grow or something?
    let mut buf = [0; 65536];
    loop {
        tokio::select! {
            from_c_message = rx_to_sock.recv() => {
                match from_c_message {
                    Some(PollnetMessage::Text(msg)) => {
                        if let Err(e) = tcp_stream.write_all(msg.as_bytes()).await {
                            debug!("TCP send error.");
                            send_error(tx_from_sock, e);
                            return Ok(());
                        }
                    },
                    Some(PollnetMessage::Binary(msg)) => {
                        if let Err(e) = tcp_stream.write_all(&msg).await {
                            debug!("TCP send error.");
                            send_error(tx_from_sock, e);
                            return Ok(());
                        }
                    },
                    Some(PollnetMessage::Disconnect) | None => {
                        return Ok(());
                    },
                    _ => {
                        warn!("Invalid message type sent to TCP socket!");
                    }
                }
            },
            _ = tcp_stream.readable() => {
                match tcp_stream.try_read(&mut buf){
                    Ok(0) => {
                        // Reading zero bytes indicates that the stream has closed
                        send_disconnect(tx_from_sock);
                        return Ok(());
                    },
                    Ok(n) => {
                        // TODO: can we avoid these copies? Does it matter?
                        debug!("TCP read {:} bytes!", n);
                        let submessage = buf[0..n].to_vec();
                        check_tx(tx_from_sock.send(PollnetMessage::Binary(submessage)))?;
                    },
                    Err(ref e) if e.kind() == tokio::io::ErrorKind::WouldBlock => {
                        // no effect?
                    },
                    Err(err) => {
                        send_error(tx_from_sock, err);
                        return Ok(())
                    },
                }
            },
        };
    }
}

async fn accept_tcp(
    mut tcp_stream: TcpStream,
    addr: SocketAddr,
    outer_tx: std::sync::mpsc::Sender<PollnetMessage>,
) {
    let (tx_to_sock, rx_to_sock) = tokio::sync::mpsc::channel(100);
    let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

    if outer_tx
        .send(PollnetMessage::NewClient(ClientConn {
            tx: tx_to_sock,
            rx: rx_from_sock,
            id: addr.to_string(),
        }))
        .is_ok()
    {
        if tcp_poll_loop(&mut tcp_stream, tx_from_sock, rx_to_sock)
            .await
            .is_err()
        {
            warn!("Unexpected poll loop termination in TCP socket.");
        }
    } else {
        warn!("TCP socket closed at weird time.");
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
                    send_error(tx_from_sock, tcp_err);
                    return;
                }
            };
            info!("TCP server waiting for connections on {}", addr);
            if tx_from_sock.send(PollnetMessage::Connect).is_err() {
                warn!("TCP server closed before finished opening.");
                return;
            }
            loop {
                tokio::select! {
                    from_c_message = rx_to_sock.recv() => {
                        match from_c_message {
                            Some(PollnetMessage::Disconnect) | None => break,
                            _ => {
                                warn!("Invalid message sent to TCP server socket!");
                            }
                        }
                    },
                    new_client = listener.accept() => {
                        match new_client {
                            Ok((tcp_stream, addr)) => {
                                tokio::spawn(accept_tcp(tcp_stream, addr, tx_from_sock.clone()));
                            },
                            Err(msg) => {
                                info!("Client failed to connect: {:?}", msg);
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

    pub fn open_tcp(&mut self, addr: String) -> SocketHandle {
        let (tx_to_sock, rx_to_sock) = tokio::sync::mpsc::channel(100);
        let (tx_from_sock, rx_from_sock) = std::sync::mpsc::channel();

        self.rt_handle.spawn(async move {
            info!("TCP client attempting to connect to {}", addr);
            match TcpStream::connect(addr).await {
                Ok(mut tcp_stream) => {
                    if tcp_poll_loop(&mut tcp_stream, tx_from_sock, rx_to_sock)
                        .await
                        .is_err()
                    {
                        warn!("Unexpected poll loop termination in TCP socket.");
                    }
                    info!("Closing TCP socket!");
                    tcp_stream.shutdown().await.unwrap_or_default(); // if this errors we don't care
                }
                Err(err) => {
                    error!("TCP client connection error: {}", err);
                    send_error(tx_from_sock, err);
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
}
