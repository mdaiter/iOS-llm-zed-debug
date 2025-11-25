use std::{
    fmt,
    io::{self, Read, Write},
    net::TcpStream,
    time::Duration,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GdbRemoteError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("remote rejected packet: {0}")]
    Remote(String),
    #[error("invalid packet checksum")]
    BadChecksum,
    #[error("unexpected reply: {0}")]
    UnexpectedReply(String),
}

#[derive(Debug, Clone)]
pub struct StopReply {
    pub signal: u8,
    pub thread_id: Option<u64>,
    pub reason: StopReason,
}

#[derive(Debug, Clone)]
pub enum StopReason {
    Breakpoint,
    Step,
    Signal,
    Unknown(String),
}

pub struct GdbRemoteClient {
    stream: TcpStream,
    pub port: u16,
    pub no_ack_mode: bool,
}

impl fmt::Debug for GdbRemoteClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GdbRemoteClient")
            .field("port", &self.port)
            .field("no_ack_mode", &self.no_ack_mode)
            .finish()
    }
}

impl GdbRemoteClient {
    pub fn connect(port: u16) -> Result<Self, GdbRemoteError> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        stream.set_write_timeout(Some(Duration::from_millis(200)))?;
        let mut client = Self {
            stream,
            port,
            no_ack_mode: false,
        };
        client.handshake()?;
        Ok(client)
    }

    fn handshake(&mut self) -> Result<(), GdbRemoteError> {
        // Drain banner if present.
        let mut tmp = [0u8; 8];
        match self.stream.read(&mut tmp) {
            Ok(_) => {}
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                // ignore
            }
            Err(err) => return Err(err.into()),
        }

        // Announce capabilities.
        let _ = self.send_packet("qSupported:multiprocess+;qRelocInsn+");
        if let Ok(reply) = self.read_packet() {
            if reply.contains("QStartNoAckMode+") {
                if let Ok(resp) = self.send_packet("QStartNoAckMode") {
                    if resp.as_deref() == Some("OK") {
                        self.no_ack_mode = true;
                    }
                }
            }
        }

        // Query current stop reason to synchronize state.
        let _ = self.send_packet("?")?;
        let _ = self.read_packet();
        Ok(())
    }

    pub fn set_software_breakpoint(&mut self, address: u64) -> Result<(), GdbRemoteError> {
        self.expect_ok(&format!("Z0,{address:x},1"))
    }

    #[allow(dead_code)]
    pub fn clear_software_breakpoint(&mut self, address: u64) -> Result<(), GdbRemoteError> {
        self.expect_ok(&format!("z0,{address:x},1"))
    }

    pub fn continue_all(&mut self) -> Result<(), GdbRemoteError> {
        self.expect_ok("vCont;c")
    }

    pub fn step_thread(&mut self, _thread_id: i64) -> Result<(), GdbRemoteError> {
        self.expect_ok("vCont;s")
    }

    pub fn wait_for_stop(&mut self) -> Result<StopReply, GdbRemoteError> {
        loop {
            let packet = self.read_packet()?;
            if let Some(reply) = parse_stop_reply(&packet) {
                return Ok(reply);
            }
        }
    }

    fn expect_ok(&mut self, payload: &str) -> Result<(), GdbRemoteError> {
        let reply = self.send_packet(payload)?;
        match reply.as_deref() {
            Some("OK") | Some("") => Ok(()),
            Some(resp) => Err(GdbRemoteError::Remote(resp.to_string())),
            None => Ok(()),
        }
    }

    fn send_packet(&mut self, payload: &str) -> Result<Option<String>, GdbRemoteError> {
        let mut packet = String::with_capacity(payload.len() + 4);
        packet.push('$');
        packet.push_str(payload);
        packet.push('#');
        let checksum = payload.bytes().fold(0u8, |acc, b| acc.wrapping_add(b));
        packet.push_str(&format!("{:02x}", checksum));
        self.stream.write_all(packet.as_bytes())?;
        self.stream.flush()?;

        if !self.no_ack_mode {
            let mut ack = [0u8; 1];
            self.stream.read_exact(&mut ack)?;
            if ack[0] != b'+' {
                return Err(GdbRemoteError::UnexpectedReply(format!(
                    "expected ack '+', got {:?}",
                    ack[0] as char
                )));
            }
        }

        if payload.starts_with('v')
            || payload.starts_with('c')
            || payload.starts_with('s')
            || payload == "?"
        {
            Ok(None)
        } else {
            self.read_packet().map(Some)
        }
    }

    fn read_packet(&mut self) -> Result<String, GdbRemoteError> {
        let mut start = [0u8; 1];
        loop {
            self.stream.read_exact(&mut start)?;
            if start[0] == b'$' {
                break;
            } else if start[0] == b'+' && self.no_ack_mode {
                continue;
            }
        }
        let mut data = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            self.stream.read_exact(&mut byte)?;
            if byte[0] == b'#' {
                break;
            }
            data.push(byte[0]);
        }
        let mut checksum_bytes = [0u8; 2];
        self.stream.read_exact(&mut checksum_bytes)?;
        let sent = u8::from_str_radix(std::str::from_utf8(&checksum_bytes).unwrap_or("00"), 16)
            .map_err(|_| GdbRemoteError::BadChecksum)?;
        let computed = data.iter().copied().fold(0u8, |acc, b| acc.wrapping_add(b));
        if sent != computed {
            return Err(GdbRemoteError::BadChecksum);
        }
        if !self.no_ack_mode {
            self.stream.write_all(b"+")?;
        }
        Ok(String::from_utf8_lossy(&data).into_owned())
    }
}

fn parse_stop_reply(reply: &str) -> Option<StopReply> {
    if reply.is_empty() {
        return None;
    }
    if reply.starts_with('S') && reply.len() >= 3 {
        let sig = u8::from_str_radix(&reply[1..3], 16).ok()?;
        return Some(StopReply {
            signal: sig,
            thread_id: None,
            reason: StopReason::Signal,
        });
    }
    if reply.starts_with('T') {
        let sig = u8::from_str_radix(&reply[1..3], 16).ok()?;
        let mut reason = StopReason::Unknown("signal".into());
        let mut thread_id = None;
        for part in reply[3..].split(';') {
            if let Some(rest) = part.strip_prefix("thread:") {
                if let Ok(id) = u64::from_str_radix(rest, 16) {
                    thread_id = Some(id);
                }
            } else if let Some(rest) = part.strip_prefix("reason:") {
                reason = match rest {
                    "breakpoint" => StopReason::Breakpoint,
                    "single-step" => StopReason::Step,
                    other => StopReason::Unknown(other.to_string()),
                };
            }
        }
        return Some(StopReply {
            signal: sig,
            thread_id,
            reason,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip() {
        // Simulate encode/decode.
        let payload = "Z0,1000,1";
        let mut buf = Vec::new();
        {
            let checksum = payload.bytes().fold(0u8, |acc, b| acc.wrapping_add(b));
            write!(&mut buf, "${payload}#{checksum:02x}").unwrap();
        }
        assert_eq!(String::from_utf8(buf.clone()).unwrap(), "$Z0,1000,1#d4");
    }

    #[test]
    fn parse_stop_reply_signal() {
        let reply = parse_stop_reply("S05").unwrap();
        assert_eq!(reply.signal, 0x05);
    }

    #[test]
    fn parse_stop_reply_thread() {
        let reply = parse_stop_reply("T05thread:1;reason:breakpoint;").unwrap();
        assert_eq!(reply.signal, 0x05);
        assert!(matches!(reply.reason, StopReason::Breakpoint));
        assert_eq!(reply.thread_id, Some(1));
    }
}
