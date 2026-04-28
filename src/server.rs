use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};

use mio::net::TcpListener;
use mio::{Events, Interest, Poll, Token};

use crate::conn::Connection;
use crate::parse::try_parse_request;
use crate::response::encode_response;
use crate::router::Handler;
use crate::types::{Method, ParseError, Response, ServerConfig, Version};

const LISTENER_TOKEN: Token = Token(0);

pub struct Server {
    listener: TcpListener,
    poll: Poll,
    events: Events,
    config: ServerConfig,
}

impl Server {
    pub fn bind<A>(addr: A) -> io::Result<Self>
    where
        A: ToSocketAddrs,
    {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "no socket address resolved"))?;
        let mut listener = TcpListener::bind(addr)?;
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut listener, LISTENER_TOKEN, Interest::READABLE)?;

        Ok(Self {
            listener,
            poll,
            events: Events::with_capacity(1024),
            config: ServerConfig::default(),
        })
    }

    pub fn with_config(mut self, config: ServerConfig) -> Self {
        self.config = config;
        self
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub fn run<H>(self, handler: H) -> io::Result<()>
    where
        H: Handler,
    {
        self.serve(handler)
    }

    pub fn serve<H>(self, handler: H) -> io::Result<()>
    where
        H: Handler,
    {
        self.serve_until(handler, || false)
    }

    pub fn serve_until<H, F>(mut self, mut handler: H, mut should_stop: F) -> io::Result<()>
    where
        H: Handler,
        F: FnMut() -> bool,
    {
        let mut connections: Vec<Option<Connection>> = Vec::new();
        let mut free_slots = Vec::new();

        loop {
            if should_stop() {
                return Ok(());
            }

            self.poll
                .poll(&mut self.events, Some(self.config.poll_timeout))?;
            let ready_events: Vec<_> = self
                .events
                .iter()
                .map(|event| (event.token(), event.is_readable(), event.is_writable()))
                .collect();

            for (token, readable, writable) in ready_events {
                if token == LISTENER_TOKEN {
                    self.accept_connections(&mut connections, &mut free_slots)?;
                    continue;
                }

                let index = token.0.saturating_sub(1);
                if index >= connections.len() || connections[index].is_none() {
                    continue;
                }

                let mut remove_connection = false;

                if readable {
                    if let Some(connection) = connections[index].as_mut() {
                        self.read_from_connection(connection, &mut handler)?;
                    }
                }

                if writable {
                    if let Some(connection) = connections[index].as_mut() {
                        self.flush_connection(connection)?;
                    }
                }

                if let Some(connection) = connections[index].as_ref() {
                    remove_connection = connection.saw_eof && !connection.has_pending_write();
                    remove_connection |=
                        connection.close_after_flush && !connection.has_pending_write();
                }

                if remove_connection {
                    self.remove_connection(index, &mut connections, &mut free_slots)?;
                } else if let Some(connection) = connections[index].as_mut() {
                    self.reregister_connection(index, connection)?;
                }
            }
        }
    }

    fn accept_connections(
        &mut self,
        connections: &mut Vec<Option<Connection>>,
        free_slots: &mut Vec<usize>,
    ) -> io::Result<()> {
        loop {
            match self.listener.accept() {
                Ok((mut socket, _peer_addr)) => {
                    let index = free_slots.pop().unwrap_or_else(|| {
                        connections.push(None);
                        connections.len() - 1
                    });

                    let token = Token(index + 1);
                    self.poll
                        .registry()
                        .register(&mut socket, token, Interest::READABLE)?;
                    connections[index] =
                        Some(Connection::new(socket, self.config.read_buffer_capacity));
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    fn read_from_connection<H>(
        &self,
        connection: &mut Connection,
        handler: &mut H,
    ) -> io::Result<()>
    where
        H: Handler,
    {
        if connection.pending_write_bytes() > self.config.max_pending_write_bytes {
            return Ok(());
        }

        let mut buffer = [0u8; 8192];
        loop {
            match connection.socket.read(&mut buffer) {
                Ok(0) => {
                    connection.saw_eof = true;
                    break;
                }
                Ok(read) => {
                    connection.read_buf.extend_from_slice(&buffer[..read]);
                    self.process_requests(connection, handler);
                    if connection.close_after_flush {
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }

        if connection.saw_eof && !connection.read_buf.is_empty() && !connection.has_pending_write()
        {
            match try_parse_request(&connection.read_buf, &self.config) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    connection.queue_write(
                        self.encode_error_response(&ParseError::BadRequest("incomplete request")),
                    );
                    connection.close_after_flush = true;
                }
                Err(error) => {
                    connection.queue_write(self.encode_error_response(&error));
                    connection.close_after_flush = true;
                }
            }
            connection.read_buf.clear();
        }

        Ok(())
    }

    fn process_requests<H>(&self, connection: &mut Connection, handler: &mut H)
    where
        H: Handler,
    {
        loop {
            match try_parse_request(&connection.read_buf, &self.config) {
                Ok(Some(parsed)) => {
                    connection.read_buf.drain(..parsed.consumed);
                    let method = parsed.request.method().clone();
                    let version = parsed.request.version();
                    let close_connection = parsed.connection_close;
                    let response = handler.handle(parsed.request);

                    let bytes = encode_response(version, &method, close_connection, response);
                    connection.queue_write(bytes);
                    connection.close_after_flush |= close_connection;

                    if close_connection {
                        connection.read_buf.clear();
                        break;
                    }

                    if connection.pending_write_bytes() > self.config.max_pending_write_bytes {
                        connection.close_after_flush = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    connection.read_buf.clear();
                    connection.queue_write(self.encode_error_response(&error));
                    connection.close_after_flush = true;
                    break;
                }
            }
        }
    }

    fn flush_connection(&self, connection: &mut Connection) -> io::Result<()> {
        while connection.has_pending_write() {
            match connection
                .socket
                .write(&connection.write_buf[connection.write_pos..])
            {
                Ok(0) => break,
                Ok(written) => {
                    connection.write_pos += written;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }

        if !connection.has_pending_write() {
            connection.write_buf.clear();
            connection.write_pos = 0;
        }

        Ok(())
    }

    fn remove_connection(
        &self,
        index: usize,
        connections: &mut [Option<Connection>],
        free_slots: &mut Vec<usize>,
    ) -> io::Result<()> {
        if let Some(mut connection) = connections[index].take() {
            self.poll.registry().deregister(&mut connection.socket)?;
            free_slots.push(index);
        }
        Ok(())
    }

    fn reregister_connection(&self, index: usize, connection: &mut Connection) -> io::Result<()> {
        let interest = if connection.has_pending_write() {
            Interest::READABLE.add(Interest::WRITABLE)
        } else {
            Interest::READABLE
        };

        self.poll
            .registry()
            .reregister(&mut connection.socket, Token(index + 1), interest)
    }

    fn encode_error_response(&self, error: &ParseError) -> Vec<u8> {
        let body = match error {
            ParseError::BadRequest(message) => *message,
            ParseError::HeaderTooLarge => "request headers are too large",
            ParseError::PayloadTooLarge => "request body is too large",
            ParseError::NotImplemented(message) => *message,
        };

        encode_response(
            Version::Http11,
            &Method::new("GET"),
            true,
            Response::text(error.status_code(), body),
        )
    }
}
