//! ANT serial message framing: SYNC, LENGTH, ID, PAYLOAD[LENGTH], XOR-CHECKSUM.

pub const SYNC: u8 = 0xa4;
const MAX_PAYLOAD: u8 = 64;

pub mod msg {
    pub const UNASSIGN_CHANNEL: u8 = 0x41;
    pub const ASSIGN_CHANNEL: u8 = 0x42;
    pub const CHANNEL_PERIOD: u8 = 0x43;
    pub const SEARCH_TIMEOUT: u8 = 0x44;
    pub const CHANNEL_FREQUENCY: u8 = 0x45;
    pub const NETWORK_KEY: u8 = 0x46;
    pub const RESET: u8 = 0x4a;
    pub const OPEN_CHANNEL: u8 = 0x4b;
    pub const CLOSE_CHANNEL: u8 = 0x4c;
    pub const REQUEST: u8 = 0x4d;
    pub const BROADCAST_DATA: u8 = 0x4e;
    pub const ACKNOWLEDGED_DATA: u8 = 0x4f;
    pub const CHANNEL_ID: u8 = 0x51;
    pub const CHANNEL_EVENT: u8 = 0x40;
    pub const LOW_PRIORITY_SEARCH_TIMEOUT: u8 = 0x63;
    pub const LIB_CONFIG: u8 = 0x6e;
}

pub mod event {
    pub const TRANSFER_TX_COMPLETED: u8 = 0x05;
    pub const TRANSFER_TX_FAILED: u8 = 0x06;
    pub const CHANNEL_CLOSED: u8 = 0x07;
    pub const RX_FAIL_GO_TO_SEARCH: u8 = 0x08;
}

#[derive(Debug, Clone, PartialEq)]
pub struct AntMessage {
    pub id: u8,
    pub data: Vec<u8>,
}

pub fn encode_message(id: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(&[SYNC, payload.len() as u8, id]);
    out.extend_from_slice(payload);
    out.push(out.iter().fold(0, |sum, b| sum ^ b));
    out
}

/// Reassembles messages from an arbitrary chunked byte stream, resyncing past corruption.
#[derive(Default)]
pub struct AntParser {
    buf: Vec<u8>,
}

impl AntParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<AntMessage> {
        self.buf.extend_from_slice(chunk);
        let buf = &self.buf;
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            while i < buf.len() && buf[i] != SYNC {
                i += 1;
            }
            if buf.len() - i < 4 {
                break;
            }
            // No ANT message is this long; a huge "length" means a stray 0xA4 inside
            // noise, and waiting for that many bytes would stall the stream.
            if buf[i + 1] > MAX_PAYLOAD {
                i += 1;
                continue;
            }
            let total = buf[i + 1] as usize + 4;
            if buf.len() - i < total {
                break;
            }
            if buf[i..i + total].iter().fold(0, |sum, b| sum ^ b) != 0 {
                // Not a real frame start; skip this byte and look for the next SYNC.
                i += 1;
                continue;
            }
            out.push(AntMessage { id: buf[i + 2], data: buf[i + 3..i + total - 1].to_vec() });
            i += total;
        }
        self.buf.drain(..i);
        out
    }
}
