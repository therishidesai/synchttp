use mio::net::TcpStream;

pub(crate) struct Connection {
    pub(crate) socket: TcpStream,
    pub(crate) read_buf: Vec<u8>,
    pub(crate) write_buf: Vec<u8>,
    pub(crate) write_pos: usize,
    pub(crate) close_after_flush: bool,
    pub(crate) saw_eof: bool,
}

impl Connection {
    pub(crate) fn new(socket: TcpStream, read_buffer_capacity: usize) -> Self {
        Self {
            socket,
            read_buf: Vec::with_capacity(read_buffer_capacity),
            write_buf: Vec::new(),
            write_pos: 0,
            close_after_flush: false,
            saw_eof: false,
        }
    }

    pub(crate) fn queue_write(&mut self, bytes: Vec<u8>) {
        if self.write_pos == self.write_buf.len() {
            self.write_buf.clear();
            self.write_pos = 0;
        }
        self.write_buf.extend_from_slice(&bytes);
    }

    pub(crate) fn has_pending_write(&self) -> bool {
        self.write_pos < self.write_buf.len()
    }

    pub(crate) fn pending_write_bytes(&self) -> usize {
        self.write_buf.len().saturating_sub(self.write_pos)
    }
}
