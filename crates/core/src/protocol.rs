//! Bounded, versioned wire protocol used by both applications.

#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

use std::fmt;
use std::io::{self, Read, Write};

use crate::{PROTOCOL_MAGIC, PROTOCOL_VERSION, SafeRelativePath};

pub const MAX_FILES: usize = 100_000;
pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_CONTROL_PAYLOAD: usize = 16 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    Hello = 1,
    HelloAck = 2,
    Offer = 3,
    Accept = 4,
    FileStart = 5,
    FileChunk = 6,
    FileEnd = 7,
    Complete = 8,
    Error = 255,
}

impl TryFrom<u8> for FrameKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Offer),
            4 => Ok(Self::Accept),
            5 => Ok(Self::FileStart),
            6 => Ok(Self::FileChunk),
            7 => Ok(Self::FileEnd),
            8 => Ok(Self::Complete),
            255 => Ok(Self::Error),
            _ => Err(ProtocolError::UnknownFrame(value)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileOffer {
    pub path: SafeRelativePath,
    pub size: u64,
    pub hash: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferOffer {
    pub id: [u8; 16],
    pub files: Vec<FileOffer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferAccept {
    pub id: [u8; 16],
    pub resume_offsets: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u16),
    UnknownFrame(u8),
    OversizedFrame(u64),
    Malformed(&'static str),
    UnsafePath(crate::UnsafePath),
    Remote(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "network I/O failed: {error}"),
            Self::BadMagic => formatter.write_str("peer used an unknown protocol"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "peer uses unsupported protocol version {version}"
                )
            }
            Self::UnknownFrame(kind) => write!(formatter, "peer sent unknown frame type {kind}"),
            Self::OversizedFrame(size) => {
                write!(formatter, "peer sent oversized frame ({size} bytes)")
            }
            Self::Malformed(reason) => write!(formatter, "peer sent malformed data: {reason}"),
            Self::UnsafePath(error) => write!(formatter, "peer sent an unsafe path: {error}"),
            Self::Remote(message) => write!(formatter, "peer rejected the transfer: {message}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::UnsafePath(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::UnsafePath> for ProtocolError {
    fn from(error: crate::UnsafePath) -> Self {
        Self::UnsafePath(error)
    }
}

pub fn write_frame(
    writer: &mut impl Write,
    kind: FrameKind,
    payload: &[u8],
) -> Result<(), ProtocolError> {
    let limit = if kind == FrameKind::FileChunk {
        MAX_CHUNK_BYTES + size_of::<u32>()
    } else {
        MAX_CONTROL_PAYLOAD
    };
    if payload.len() > limit {
        return Err(ProtocolError::OversizedFrame(payload.len() as u64));
    }

    writer.write_all(&PROTOCOL_MAGIC)?;
    writer.write_all(&PROTOCOL_VERSION.to_be_bytes())?;
    writer.write_all(&[kind as u8])?;
    writer.write_all(&(payload.len() as u64).to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame(reader: &mut impl Read) -> Result<Frame, ProtocolError> {
    let mut magic = [0_u8; 4];
    reader.read_exact(&mut magic)?;
    if magic != PROTOCOL_MAGIC {
        return Err(ProtocolError::BadMagic);
    }

    let version = read_u16(reader)?;
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(version));
    }

    let mut kind = [0_u8; 1];
    reader.read_exact(&mut kind)?;
    let kind = FrameKind::try_from(kind[0])?;
    let length = read_u64(reader)?;
    let limit = if kind == FrameKind::FileChunk {
        (MAX_CHUNK_BYTES + size_of::<u32>()) as u64
    } else {
        MAX_CONTROL_PAYLOAD as u64
    };
    if length > limit {
        return Err(ProtocolError::OversizedFrame(length));
    }

    let length = usize::try_from(length)
        .map_err(|_| ProtocolError::Malformed("frame length exceeds platform limits"))?;
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    if kind == FrameKind::Error {
        return Err(ProtocolError::Remote(decode_error(&payload)?));
    }
    Ok(Frame { kind, payload })
}

pub fn encode_offer(offer: &TransferOffer) -> Result<Vec<u8>, ProtocolError> {
    if offer.files.len() > MAX_FILES {
        return Err(ProtocolError::Malformed("too many files"));
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(&offer.id);
    push_u32(&mut payload, offer.files.len())?;
    for file in &offer.files {
        let path = file
            .path
            .as_path()
            .to_str()
            .ok_or(ProtocolError::Malformed("path is not valid Unicode"))?;
        let path = path.replace('\\', "/");
        push_string(&mut payload, &path)?;
        payload.extend_from_slice(&file.size.to_be_bytes());
        payload.extend_from_slice(&file.hash);
    }
    Ok(payload)
}

pub fn decode_offer(payload: &[u8]) -> Result<TransferOffer, ProtocolError> {
    let mut decoder = Decoder::new(payload);
    let id = decoder.array()?;
    let count = decoder.u32()? as usize;
    if count > MAX_FILES {
        return Err(ProtocolError::Malformed("too many files"));
    }

    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        let wire_path = decoder.string()?;
        let path = SafeRelativePath::new(std::path::Path::new(&wire_path))?;
        let size = decoder.u64()?;
        let hash = decoder.array()?;
        files.push(FileOffer { path, size, hash });
    }
    decoder.finish()?;
    Ok(TransferOffer { id, files })
}

pub fn encode_accept(accept: &TransferAccept) -> Result<Vec<u8>, ProtocolError> {
    if accept.resume_offsets.len() > MAX_FILES {
        return Err(ProtocolError::Malformed("too many resume offsets"));
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(&accept.id);
    push_u32(&mut payload, accept.resume_offsets.len())?;
    for offset in &accept.resume_offsets {
        payload.extend_from_slice(&offset.to_be_bytes());
    }
    Ok(payload)
}

pub fn decode_accept(payload: &[u8]) -> Result<TransferAccept, ProtocolError> {
    let mut decoder = Decoder::new(payload);
    let id = decoder.array()?;
    let count = decoder.u32()? as usize;
    if count > MAX_FILES {
        return Err(ProtocolError::Malformed("too many resume offsets"));
    }
    let mut resume_offsets = Vec::with_capacity(count);
    for _ in 0..count {
        resume_offsets.push(decoder.u64()?);
    }
    decoder.finish()?;
    Ok(TransferAccept { id, resume_offsets })
}

pub fn encode_file_start(index: u32, offset: u64) -> Vec<u8> {
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&index.to_be_bytes());
    payload.extend_from_slice(&offset.to_be_bytes());
    payload
}

pub fn decode_file_start(payload: &[u8]) -> Result<(u32, u64), ProtocolError> {
    let mut decoder = Decoder::new(payload);
    let value = (decoder.u32()?, decoder.u64()?);
    decoder.finish()?;
    Ok(value)
}

pub fn encode_file_chunk(index: u32, bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if bytes.len() > MAX_CHUNK_BYTES {
        return Err(ProtocolError::OversizedFrame(bytes.len() as u64));
    }
    let mut payload = Vec::with_capacity(size_of::<u32>() + bytes.len());
    payload.extend_from_slice(&index.to_be_bytes());
    payload.extend_from_slice(bytes);
    Ok(payload)
}

pub fn decode_file_chunk(payload: &[u8]) -> Result<(u32, &[u8]), ProtocolError> {
    if payload.len() < size_of::<u32>() {
        return Err(ProtocolError::Malformed("short file chunk"));
    }
    let index = u32::from_be_bytes(
        payload[..4]
            .try_into()
            .map_err(|_| ProtocolError::Malformed("short file chunk index"))?,
    );
    Ok((index, &payload[4..]))
}

pub fn encode_file_end(index: u32, hash: &[u8; 32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(36);
    payload.extend_from_slice(&index.to_be_bytes());
    payload.extend_from_slice(hash);
    payload
}

pub fn decode_file_end(payload: &[u8]) -> Result<(u32, [u8; 32]), ProtocolError> {
    let mut decoder = Decoder::new(payload);
    let value = (decoder.u32()?, decoder.array()?);
    decoder.finish()?;
    Ok(value)
}

pub fn encode_complete(file_count: u32) -> Vec<u8> {
    file_count.to_be_bytes().to_vec()
}

pub fn decode_complete(payload: &[u8]) -> Result<u32, ProtocolError> {
    let mut decoder = Decoder::new(payload);
    let count = decoder.u32()?;
    decoder.finish()?;
    Ok(count)
}

pub fn encode_error(message: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = Vec::new();
    push_string(&mut payload, message)?;
    Ok(payload)
}

fn decode_error(payload: &[u8]) -> Result<String, ProtocolError> {
    let mut decoder = Decoder::new(payload);
    let message = decoder.string()?;
    decoder.finish()?;
    Ok(message)
}

fn push_u32(output: &mut Vec<u8>, value: usize) -> Result<(), ProtocolError> {
    let value = u32::try_from(value).map_err(|_| ProtocolError::Malformed("value exceeds u32"))?;
    output.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn push_string(output: &mut Vec<u8>, value: &str) -> Result<(), ProtocolError> {
    if value.len() > MAX_PATH_BYTES {
        return Err(ProtocolError::Malformed("string is too long"));
    }
    push_u32(output, value.len())?;
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_u16(reader: &mut impl Read) -> Result<u16, io::Error> {
    let mut bytes = [0_u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, io::Error> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

struct Decoder<'a> {
    remaining: &'a [u8],
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ProtocolError> {
        if length > self.remaining.len() {
            return Err(ProtocolError::Malformed("truncated payload"));
        }
        let (value, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ProtocolError::Malformed("truncated u32"))?,
        ))
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ProtocolError::Malformed("truncated u64"))?,
        ))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ProtocolError::Malformed("truncated fixed-size value"))
    }

    fn string(&mut self) -> Result<String, ProtocolError> {
        let length = self.u32()? as usize;
        if length > MAX_PATH_BYTES {
            return Err(ProtocolError::Malformed("string is too long"));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| ProtocolError::Malformed("string is not valid UTF-8"))
    }

    fn finish(self) -> Result<(), ProtocolError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::Malformed("payload has trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use super::*;

    fn example_offer() -> TransferOffer {
        TransferOffer {
            id: [7; 16],
            files: vec![FileOffer {
                path: SafeRelativePath::new(Path::new("folder/file.txt")).unwrap(),
                size: 42,
                hash: [9; 32],
            }],
        }
    }

    #[test]
    fn offer_round_trips() {
        let offer = example_offer();
        let encoded = encode_offer(&offer).unwrap();
        assert_eq!(decode_offer(&encoded).unwrap(), offer);
    }

    #[test]
    fn frame_round_trips() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, FrameKind::Hello, b"hello").unwrap();
        let frame = read_frame(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(frame.kind, FrameKind::Hello);
        assert_eq!(frame.payload, b"hello");
    }

    #[test]
    fn offer_rejects_traversal() {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&[0; 16]);
        encoded.extend_from_slice(&1_u32.to_be_bytes());
        push_string(&mut encoded, "../escape").unwrap();
        encoded.extend_from_slice(&0_u64.to_be_bytes());
        encoded.extend_from_slice(&[0; 32]);
        assert!(matches!(
            decode_offer(&encoded),
            Err(ProtocolError::UnsafePath(_))
        ));
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&PROTOCOL_MAGIC);
        bytes.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        bytes.push(FrameKind::Hello as u8);
        bytes.extend_from_slice(&((MAX_CONTROL_PAYLOAD as u64) + 1).to_be_bytes());
        assert!(matches!(
            read_frame(&mut Cursor::new(bytes)),
            Err(ProtocolError::OversizedFrame(_))
        ));
    }
}
