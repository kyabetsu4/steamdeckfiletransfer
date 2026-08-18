//! PC-side file collection and streaming sender.

#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::SafeRelativePath;
use crate::protocol::{
    FileOffer, FrameKind, MAX_CHUNK_BYTES, ProtocolError, TransferOffer, decode_accept,
    decode_complete, encode_complete, encode_file_chunk, encode_file_end, encode_file_start,
    encode_offer, read_frame, write_frame,
};

#[derive(Clone, Debug)]
pub enum SendEvent {
    Preparing(PathBuf),
    Connected(SocketAddr),
    TransferStarted { files: usize, bytes: u64 },
    FileStarted { path: PathBuf, size: u64 },
    Progress { sent: u64, total: u64 },
    Complete { files: usize, bytes: u64 },
}

#[derive(Debug)]
pub enum SendError {
    Io(io::Error),
    Protocol(ProtocolError),
    InvalidInput(String),
    SourceChanged(PathBuf),
}

impl fmt::Display for SendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "file I/O failed: {error}"),
            Self::Protocol(error) => error.fmt(formatter),
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::SourceChanged(path) => write!(
                formatter,
                "source file changed while it was being sent: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::InvalidInput(_) | Self::SourceChanged(_) => None,
        }
    }
}

impl From<io::Error> for SendError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for SendError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

struct SourceFile {
    source: PathBuf,
    offer: FileOffer,
}

pub fn send_paths(
    address: SocketAddr,
    inputs: &[PathBuf],
    mut on_event: impl FnMut(SendEvent),
) -> Result<(), SendError> {
    let sources = collect_sources(inputs, &mut on_event)?;
    let total_bytes = sources.iter().try_fold(0_u64, |total, source| {
        total
            .checked_add(source.offer.size)
            .ok_or_else(|| SendError::InvalidInput("transfer size exceeds u64".to_owned()))
    })?;
    let transfer_id = create_transfer_id(&sources);
    let offer = TransferOffer {
        id: transfer_id,
        files: sources.iter().map(|source| source.offer.clone()).collect(),
    };

    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_mins(1)))?;
    on_event(SendEvent::Connected(address));

    write_frame(&mut stream, FrameKind::Hello, &[])?;
    expect_frame(&mut stream, FrameKind::HelloAck)?;
    write_frame(&mut stream, FrameKind::Offer, &encode_offer(&offer)?)?;

    let accept_frame = expect_frame(&mut stream, FrameKind::Accept)?;
    let accept = decode_accept(&accept_frame)?;
    if accept.id != transfer_id {
        return Err(ProtocolError::Malformed("accept used a different transfer ID").into());
    }
    if accept.resume_offsets.len() != sources.len() {
        return Err(ProtocolError::Malformed("resume offset count does not match offer").into());
    }

    on_event(SendEvent::TransferStarted {
        files: sources.len(),
        bytes: total_bytes,
    });

    let mut transferred = 0_u64;
    let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
    for (index, (source, offset)) in sources
        .iter()
        .zip(accept.resume_offsets.iter().copied())
        .enumerate()
    {
        if offset > source.offer.size {
            return Err(ProtocolError::Malformed("resume offset exceeds file size").into());
        }
        let index = u32::try_from(index)
            .map_err(|_| SendError::InvalidInput("too many files".to_owned()))?;
        on_event(SendEvent::FileStarted {
            path: source.offer.path.as_path().to_path_buf(),
            size: source.offer.size,
        });

        let mut file = File::open(&source.source)?;
        let current_size = file.metadata()?.len();
        if current_size != source.offer.size {
            return Err(SendError::SourceChanged(source.source.clone()));
        }
        file.seek(SeekFrom::Start(offset))?;
        write_frame(
            &mut stream,
            FrameKind::FileStart,
            &encode_file_start(index, offset),
        )?;

        let mut position = offset;
        transferred = transferred
            .checked_add(offset)
            .ok_or_else(|| SendError::InvalidInput("transfer progress overflow".to_owned()))?;
        on_event(SendEvent::Progress {
            sent: transferred,
            total: total_bytes,
        });

        while position < source.offer.size {
            let remaining = source.offer.size - position;
            let wanted = usize::try_from(remaining.min(MAX_CHUNK_BYTES as u64))
                .map_err(|_| SendError::InvalidInput("chunk size overflow".to_owned()))?;
            let read = file.read(&mut buffer[..wanted])?;
            if read == 0 {
                return Err(SendError::SourceChanged(source.source.clone()));
            }
            let payload = encode_file_chunk(index, &buffer[..read])?;
            write_frame(&mut stream, FrameKind::FileChunk, &payload)?;
            position += read as u64;
            transferred += read as u64;
            on_event(SendEvent::Progress {
                sent: transferred,
                total: total_bytes,
            });
        }

        write_frame(
            &mut stream,
            FrameKind::FileEnd,
            &encode_file_end(index, &source.offer.hash),
        )?;
    }

    write_frame(
        &mut stream,
        FrameKind::Complete,
        &encode_complete(
            u32::try_from(sources.len())
                .map_err(|_| SendError::InvalidInput("too many files".to_owned()))?,
        ),
    )?;
    let completed = decode_complete(&expect_frame(&mut stream, FrameKind::Complete)?)?;
    if completed as usize != sources.len() {
        return Err(ProtocolError::Malformed("completion count does not match offer").into());
    }

    on_event(SendEvent::Complete {
        files: sources.len(),
        bytes: total_bytes,
    });
    Ok(())
}

fn collect_sources(
    inputs: &[PathBuf],
    on_event: &mut impl FnMut(SendEvent),
) -> Result<Vec<SourceFile>, SendError> {
    if inputs.is_empty() {
        return Err(SendError::InvalidInput(
            "choose at least one file or folder".to_owned(),
        ));
    }

    let mut sources = Vec::new();
    let mut wire_paths = HashSet::new();
    for input in inputs {
        let metadata = std::fs::symlink_metadata(input).map_err(|error| {
            SendError::InvalidInput(format!("cannot inspect {}: {error}", input.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(SendError::InvalidInput(format!(
                "symbolic links are not supported: {}",
                input.display()
            )));
        }
        let name = input.file_name().ok_or_else(|| {
            SendError::InvalidInput(format!("path has no filename: {}", input.display()))
        })?;
        let relative_root = PathBuf::from(name);
        if metadata.is_file() {
            add_source(
                input,
                &relative_root,
                &mut sources,
                &mut wire_paths,
                on_event,
            )?;
        } else if metadata.is_dir() {
            collect_directory(
                input,
                &relative_root,
                &mut sources,
                &mut wire_paths,
                on_event,
            )?;
        } else {
            return Err(SendError::InvalidInput(format!(
                "unsupported file type: {}",
                input.display()
            )));
        }
    }
    if sources.is_empty() {
        return Err(SendError::InvalidInput(
            "the selected folders contain no regular files".to_owned(),
        ));
    }
    sources.sort_by(|left, right| left.offer.path.as_path().cmp(right.offer.path.as_path()));
    Ok(sources)
}

fn collect_directory(
    directory: &Path,
    relative: &Path,
    sources: &mut Vec<SourceFile>,
    wire_paths: &mut HashSet<PathBuf>,
    on_event: &mut impl FnMut(SendEvent),
) -> Result<(), SendError> {
    let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let metadata = entry.file_type()?;
        let source = entry.path();
        let child_relative = relative.join(entry.file_name());
        if metadata.is_symlink() {
            return Err(SendError::InvalidInput(format!(
                "symbolic links are not supported: {}",
                source.display()
            )));
        }
        if metadata.is_dir() {
            collect_directory(&source, &child_relative, sources, wire_paths, on_event)?;
        } else if metadata.is_file() {
            add_source(&source, &child_relative, sources, wire_paths, on_event)?;
        } else {
            return Err(SendError::InvalidInput(format!(
                "unsupported file type: {}",
                source.display()
            )));
        }
    }
    Ok(())
}

fn add_source(
    source: &Path,
    relative: &Path,
    sources: &mut Vec<SourceFile>,
    wire_paths: &mut HashSet<PathBuf>,
    on_event: &mut impl FnMut(SendEvent),
) -> Result<(), SendError> {
    let safe_path = SafeRelativePath::new(relative).map_err(|error| {
        SendError::InvalidInput(format!("unsafe input path {}: {error}", relative.display()))
    })?;
    if !wire_paths.insert(safe_path.as_path().to_path_buf()) {
        return Err(SendError::InvalidInput(format!(
            "two inputs produce the same destination path: {}",
            safe_path.as_path().display()
        )));
    }
    on_event(SendEvent::Preparing(source.to_path_buf()));
    let size = source.metadata()?.len();
    let hash = hash_file(source)?;
    sources.push(SourceFile {
        source: source.to_path_buf(),
        offer: FileOffer {
            path: safe_path,
            size,
            hash,
        },
    });
    Ok(())
}

fn hash_file(path: &Path) -> Result<[u8; 32], io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn create_transfer_id(sources: &[SourceFile]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    hasher.update(&timestamp.to_be_bytes());
    hasher.update(&std::process::id().to_be_bytes());
    for source in sources {
        hasher.update(&source.offer.hash);
        hasher.update(&source.offer.size.to_be_bytes());
    }
    let mut id = [0_u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    id
}

fn expect_frame(stream: &mut TcpStream, expected: FrameKind) -> Result<Vec<u8>, SendError> {
    let frame = read_frame(stream)?;
    if frame.kind != expected {
        return Err(ProtocolError::Malformed("peer sent an unexpected frame").into());
    }
    Ok(frame.payload)
}
