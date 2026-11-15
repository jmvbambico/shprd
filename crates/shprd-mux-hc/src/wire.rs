//! The bytes on the pipe: plain JSON lines until `hello`, then framed messages.
//!
//! `hc connect` starts in plain mode — one JSON object per line — and switches
//! to the framed protocol the moment it *parses* the reply to
//! `hello {framed:true}`. Any byte read after that reply is already framed, so
//! the switch has to happen inside the read loop, not after the caller's
//! `await`. That trap cost the M7ef spike its first client (it hung), so the
//! switch lives here, in [`Parser`], where a fixture can prove it: the same
//! buffer holding the hello line plus framed bytes must parse as one line then
//! as frames.
//!
//! A frame is `u32le channel | u8 kind | u32le length | payload`; channel 0
//! carries JSON-RPC, a stream channel carries terminal bytes or event lines.
//! Every data frame a client is sent has to be answered with a credit frame
//! carrying its length, or the core stops sending (protocol.ts in Shepherdr,
//! `crates/herdr-core` in herdr-core itself).

use serde_json::Value;

use crate::error::{Error, Result};

/// The largest frame either side may send (16 MiB, as herdr-core's `protocol.ts`
/// says). A header claiming more is refused before anything is allocated.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// What a frame is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Bytes: JSON on channel 0, terminal output or event lines on a stream.
    Data = 0,
    /// The receiver acknowledging `length` bytes of a data frame.
    Credit = 1,
    /// A stream ending.
    Close = 2,
    /// `u16le cols, u16le rows`.
    Resize = 3,
    /// A focus change.
    Focus = 4,
    /// Facts about a stream.
    Facts = 5,
}

impl FrameKind {
    /// The byte this kind is written as.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// The kind a byte names, or `None` for a kind this build does not know.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Data),
            1 => Some(Self::Credit),
            2 => Some(Self::Close),
            3 => Some(Self::Resize),
            4 => Some(Self::Focus),
            5 => Some(Self::Facts),
            _ => None,
        }
    }
}

/// One thing that arrived from the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The core switched to framed mode; every byte after this is a frame.
    Upgraded,
    /// One JSON-RPC message from channel 0: a reply, or a notification.
    Control(Value),
    /// Bytes on a stream channel (terminal output, or the event stream's JSON
    /// lines).
    Data {
        /// The stream channel.
        channel: u32,
        /// The bytes the core sent.
        payload: Vec<u8>,
    },
    /// The core closed a stream channel; `payload` carries the reason, if any.
    Close {
        /// The stream channel.
        channel: u32,
        /// The reason, usually an empty payload.
        payload: Vec<u8>,
    },
    /// The core acknowledged `bytes` bytes of a data frame we sent.
    Credit {
        /// The stream channel.
        channel: u32,
        /// How many bytes it acknowledged.
        bytes: u32,
    },
    /// A stream frame of a kind this crate does not model yet.
    Other {
        /// The stream channel.
        channel: u32,
        /// Which kind it was.
        kind: FrameKind,
        /// Its payload.
        payload: Vec<u8>,
    },
}

/// Frame head sizes: channel, kind, length.
const HEAD: usize = 9;

impl Incoming {
    /// The channel this arrived on, when it arrived on one.
    pub fn channel(&self) -> Option<u32> {
        match self {
            Self::Upgraded | Self::Control(_) => None,
            Self::Data { channel, .. }
            | Self::Close { channel, .. }
            | Self::Credit { channel, .. }
            | Self::Other { channel, .. } => Some(*channel),
        }
    }
}

/// One JSON object as a plain line, for the `hello` call and nothing else.
pub fn encode_plain(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| Error::Output(format!("cannot encode a plain message: {error}")))?;
    if bytes.len() > MAX_FRAME {
        return Err(Error::Output("a plain message exceeds 16 MiB".into()));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// One frame, ready to write.
pub fn encode_frame(channel: u32, kind: FrameKind, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > MAX_FRAME {
        return Err(Error::Output(format!(
            "a frame of {} bytes exceeds the 16 MiB cap",
            payload.len()
        )));
    }
    let mut bytes = Vec::with_capacity(HEAD + payload.len());
    bytes.extend_from_slice(&channel.to_le_bytes());
    bytes.push(kind.code());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

/// A credit frame acknowledging `bytes` bytes on `channel`.
pub fn credit_frame(channel: u32, bytes: usize) -> Result<Vec<u8>> {
    let acked =
        u32::try_from(bytes).map_err(|_| Error::Output("a credit is larger than u32".into()))?;
    encode_frame(channel, FrameKind::Credit, &acked.to_le_bytes())
}

/// The incremental reader: plain lines, then frames.
///
/// Bytes may be handed to [`Parser::push`] in any split — a read need not end
/// on a line or a frame boundary — and the order of what comes back is the
/// order it arrived in.
#[derive(Debug)]
pub struct Parser {
    /// The `hello` request id, whose reply flips the mode.
    hello_id: u64,
    framed: bool,
    plain: Vec<u8>,
    frames: Vec<u8>,
}

impl Parser {
    /// A parser waiting for the reply to the `hello` request with `hello_id`.
    pub fn new(hello_id: u64) -> Self {
        Self {
            hello_id,
            framed: false,
            plain: Vec::new(),
            frames: Vec::new(),
        }
    }

    /// Whether the framed protocol has begun.
    pub fn is_framed(&self) -> bool {
        self.framed
    }

    /// Feed bytes read from the core and take what they completed.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Incoming>> {
        let mut out = Vec::new();
        if self.framed {
            self.frames.extend_from_slice(bytes);
            self.drain_frames(&mut out)?;
            return Ok(out);
        }
        self.plain.extend_from_slice(bytes);
        loop {
            let Some(end) = self.plain.iter().position(|byte| *byte == b'\n') else {
                if self.plain.len() > MAX_FRAME {
                    return Err(Error::Protocol(
                        "the core sent a plain line longer than 16 MiB".into(),
                    ));
                }
                return Ok(out);
            };
            let line: Vec<u8> = self.plain.drain(..=end).collect();
            let value: Value =
                serde_json::from_slice(&line[..line.len() - 1]).map_err(|error| {
                    Error::Protocol(format!("the core sent a line that is not JSON: {error}"))
                })?;
            if self.is_hello_reply(&value) {
                // The switch happens as this line is parsed: everything after
                // it is framed, whether or not it has arrived yet.
                self.framed = true;
                out.push(Incoming::Control(value));
                out.push(Incoming::Upgraded);
                let rest = std::mem::take(&mut self.plain);
                self.frames.extend_from_slice(&rest);
                self.drain_frames(&mut out)?;
                return Ok(out);
            }
            out.push(Incoming::Control(value));
        }
    }

    /// Whether `value` is this parser's `hello` reply announcing framed mode.
    fn is_hello_reply(&self, value: &Value) -> bool {
        value.get("id").and_then(Value::as_u64) == Some(self.hello_id)
            && value
                .get("result")
                .and_then(|result| result.get("framed"))
                .and_then(Value::as_bool)
                == Some(true)
    }

    /// Take every complete frame in the buffer.
    fn drain_frames(&mut self, out: &mut Vec<Incoming>) -> Result<()> {
        loop {
            if self.frames.len() < HEAD {
                return Ok(());
            }
            let channel = u32::from_le_bytes([
                self.frames[0],
                self.frames[1],
                self.frames[2],
                self.frames[3],
            ]);
            let code = self.frames[4];
            let length = u32::from_le_bytes([
                self.frames[5],
                self.frames[6],
                self.frames[7],
                self.frames[8],
            ]) as usize;
            if length > MAX_FRAME {
                return Err(Error::Protocol(format!(
                    "the core announced a {length}-byte frame, over the 16 MiB cap"
                )));
            }
            if self.frames.len() < HEAD + length {
                return Ok(());
            }
            let frame: Vec<u8> = self.frames.drain(..HEAD + length).collect();
            let payload = frame[HEAD..].to_vec();
            let kind = FrameKind::from_code(code)
                .ok_or_else(|| Error::Protocol(format!("the core sent frame kind {code}")))?;
            out.push(match kind {
                FrameKind::Data if channel == 0 => {
                    let value = serde_json::from_slice(&payload).map_err(|error| {
                        Error::Protocol(format!("channel 0 sent a frame that is not JSON: {error}"))
                    })?;
                    Incoming::Control(value)
                }
                FrameKind::Data => Incoming::Data { channel, payload },
                FrameKind::Close => Incoming::Close { channel, payload },
                FrameKind::Credit => {
                    let bytes = payload
                        .first_chunk::<4>()
                        .map(|bytes| u32::from_le_bytes(*bytes));
                    let bytes = bytes.ok_or_else(|| {
                        Error::Protocol("a credit frame was not 4 bytes".to_string())
                    })?;
                    Incoming::Credit { channel, bytes }
                }
                other => Incoming::Other {
                    channel,
                    kind: other,
                    payload,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    /// The `hello` exchange as M7ef recorded it, then one `rpc.discover` call
    /// answered on channel 0.
    fn hello_reply(id: u64) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"core":{{"id":"06f3e992","name":"mac"}},"generation":1,"framed":true,"version":"0.1.21","features":["framed","events"],"methods":["hello","doc.get"]}}}}"#
        )
    }

    #[test]
    fn one_frame_round_trips() {
        let bytes = encode_frame(0, FrameKind::Data, b"{}").unwrap();
        assert_eq!(bytes.len(), HEAD + 2);
        assert_eq!(&bytes[..4], &0u32.to_le_bytes());
        assert_eq!(bytes[4], 0);
        assert_eq!(&bytes[5..9], &2u32.to_le_bytes());
        assert_eq!(&bytes[9..], b"{}");
    }

    #[test]
    fn the_hello_line_flips_the_parser_before_the_bytes_after_it() {
        let mut parser = Parser::new(1);
        let mut bytes = hello_reply(1).into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(&encode_frame(0, FrameKind::Data, br#"{"id":1}"#).unwrap());
        let got = parser.push(&bytes).unwrap();
        assert!(parser.is_framed());
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(
            got[0],
            Incoming::Control(serde_json::from_str::<Value>(&hello_reply(1)).unwrap())
        );
        assert_eq!(got[1], Incoming::Upgraded);
        // The reply that arrives in the same buffer as the hello line was
        // framed, and is read as a frame, not as another plain line.
        assert_eq!(got[2], Incoming::Control(json!({"id": 1})));
    }

    #[test]
    fn a_reply_to_another_id_does_not_flip_the_parser() {
        let mut parser = Parser::new(1);
        let mut bytes = hello_reply(2).into_bytes();
        bytes.push(b'\n');
        let got = parser.push(&bytes).unwrap();
        assert!(!parser.is_framed());
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], Incoming::Control(_)));
    }

    #[test]
    fn a_split_read_yields_the_same_messages_as_one() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(hello_reply(1).as_bytes());
        bytes.push(b'\n');
        bytes.extend_from_slice(
            &encode_frame(0, FrameKind::Data, br#"{"id":2,"result":1}"#).unwrap(),
        );
        bytes.extend_from_slice(&encode_frame(3, FrameKind::Data, b"paint").unwrap());
        bytes.extend_from_slice(&credit_frame(3, 5).unwrap());
        bytes.extend_from_slice(&encode_frame(3, FrameKind::Close, b"").unwrap());

        let mut whole = Parser::new(1);
        let expected = whole.push(&bytes).unwrap();
        assert_eq!(expected.len(), 6, "{expected:?}");

        for split in 1..bytes.len() {
            let mut parser = Parser::new(1);
            let mut got = parser.push(&bytes[..split]).unwrap();
            got.extend(parser.push(&bytes[split..]).unwrap());
            assert_eq!(got, expected, "split at {split}");
            assert!(parser.is_framed(), "split at {split}");
        }

        let mut parser = Parser::new(1);
        let mut got = Vec::new();
        for byte in &bytes {
            got.extend(parser.push(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(got, expected);
        assert!(parser.is_framed());
    }

    #[test]
    fn stream_frames_keep_their_channel_and_kind() {
        let mut parser = Parser::new(1);
        let mut bytes = hello_reply(1).into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(&encode_frame(7, FrameKind::Data, b"hello").unwrap());
        bytes.extend_from_slice(&credit_frame(7, 5).unwrap());
        bytes.extend_from_slice(&encode_frame(7, FrameKind::Close, b"").unwrap());
        bytes.extend_from_slice(&encode_frame(7, FrameKind::Resize, &[80, 0, 24, 0]).unwrap());
        let got = parser.push(&bytes).unwrap();
        assert_eq!(
            got,
            vec![
                Incoming::Control(serde_json::from_str::<Value>(&hello_reply(1)).unwrap()),
                Incoming::Upgraded,
                Incoming::Data {
                    channel: 7,
                    payload: b"hello".to_vec()
                },
                Incoming::Credit {
                    channel: 7,
                    bytes: 5
                },
                Incoming::Close {
                    channel: 7,
                    payload: Vec::new()
                },
                Incoming::Other {
                    channel: 7,
                    kind: FrameKind::Resize,
                    payload: vec![80, 0, 24, 0]
                },
            ]
        );
    }

    #[test]
    fn an_oversized_frame_is_refused_before_allocating() {
        let mut parser = Parser::new(1);
        let mut bytes = hello_reply(1).into_bytes();
        bytes.push(b'\n');
        let mut header = Vec::new();
        header.extend_from_slice(&0u32.to_le_bytes());
        header.push(0);
        header.extend_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        bytes.extend_from_slice(&header);
        let error = parser.push(&bytes).unwrap_err();
        assert!(matches!(error, Error::Protocol(_)), "{error}");
    }

    #[test]
    fn frames_arriving_before_the_newline_that_ends_the_hello_line_still_parse() {
        // A core may write the hello reply without flushing a newline and the
        // framed bytes after it in the same write. The mode switches when the
        // reply is parsed, so those bytes must not be read as part of a line.
        let mut parser = Parser::new(1);
        let mut bytes = hello_reply(1).into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(&encode_frame(0, FrameKind::Data, br#"{"id":2}"#).unwrap());
        let got = parser.push(&bytes).unwrap();
        assert_eq!(got.len(), 3);
        assert!(matches!(got[2], Incoming::Control(ref value) if value["id"] == 2));

        // And the same bytes split so the reply and its newline arrive alone.
        let mut parser = Parser::new(1);
        let mut hello = hello_reply(1).into_bytes();
        hello.push(b'\n');
        parser.push(&hello).unwrap();
        assert!(parser.is_framed());
        let got = parser
            .push(&encode_frame(0, FrameKind::Data, br#"{"id":2}"#).unwrap())
            .unwrap();
        assert!(matches!(got[0], Incoming::Control(ref value) if value["id"] == 2));
    }

    #[test]
    fn a_line_that_is_not_json_is_a_protocol_error() {
        let mut parser = Parser::new(1);
        let error = parser.push(b"not json\n").unwrap_err();
        assert!(matches!(error, Error::Protocol(_)), "{error}");
    }

    #[test]
    fn a_credit_that_is_not_four_bytes_is_a_protocol_error() {
        let mut parser = Parser::new(1);
        let mut bytes = hello_reply(1).into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(&encode_frame(3, FrameKind::Credit, b"ab").unwrap());
        assert!(matches!(parser.push(&bytes), Err(Error::Protocol(_))));
    }
}
