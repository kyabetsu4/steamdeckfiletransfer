//! Steam Deck-side safe and resumable file receiver.

#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::protocol::{
    FileOffer, FrameKind, ProtocolError, TransferAccept, TransferOffer, decode_complete,
    decode_file_chunk, decode_file_end, decode_file_start, decode_offer, encode_accept,
    encode_complete, read_frame, write_frame,
};

#[derive(Clone, Debug)]
pub enum ReceiveEvent {
    Listening(SocketAddr),
    Connected(SocketAddr),
    TransferOffered { files: usize, bytes: u64 },
    FileStarted { path: PathBuf, size: u64 },
    Progress { received: u64, total: u64 },
    FileCompleted { path: PathBuf },
    Failed { message: String },
    Complete { files: usize, bytes: u64 },
}

#[derive(Debug)]
pub enum ReceiveError {
    Io(io::Error),
    Protocol(ProtocolError),
    UnsafeDestination(String),
    HashMismatch(PathBuf),
}

impl fmt::Display for ReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "file I/O failed: {error}"),
            Self::Protocol(error) => error.fmt(formatter),
            Self::UnsafeDestination(message) => formatter.write_str(message),
            Self::HashMismatch(path) => {
                write!(
                    formatter,
                    "received data failed hash verification: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ReceiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::UnsafeDestination(_) | Self::HashMismatch(_) => None,
        }
    }
}

impl From<io::Error> for ReceiveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for ReceiveError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

struct ReceiveFile {
    offer: FileOffer,
    partial: PathBuf,
    destination: PathBuf,
    resume_offset: u64,
}

pub fn listen(
    address: SocketAddr,
    output: &Path,
    mut on_event: impl FnMut(ReceiveEvent),
) -> Result<(), ReceiveError> {
    let root = prepare_root(output)?;
    let listener = TcpListener::bind(address)?;
    on_event(ReceiveEvent::Listening(listener.local_addr()?));
    loop {
        let (stream, peer) = listener.accept()?;
        on_event(ReceiveEvent::Connected(peer));
        if let Err(error) = receive_stream(stream, &root, &mut on_event) {
            on_event(ReceiveEvent::Failed {
                message: format!("Transfer from {peer} failed: {error}"),
            });
        }
    }
}

pub fn receive_once(
    listener: &TcpListener,
    output: &Path,
    mut on_event: impl FnMut(ReceiveEvent),
) -> Result<(), ReceiveError> {
    let root = prepare_root(output)?;
    let (stream, peer) = listener.accept()?;
    on_event(ReceiveEvent::Connected(peer));
    receive_stream(stream, &root, &mut on_event)
}

fn receive_stream(
    mut stream: TcpStream,
    root: &Path,
    on_event: &mut impl FnMut(ReceiveEvent),
) -> Result<(), ReceiveError> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_mins(1)))?;

    expect_empty(&mut stream, FrameKind::Hello)?;
    write_frame(&mut stream, FrameKind::HelloAck, &[])?;
    let offer_frame = expect_frame(&mut stream, FrameKind::Offer)?;
    let offer = decode_offer(&offer_frame)?;
    if offer.files.is_empty() {
        return Err(ProtocolError::Malformed("offer contains no files").into());
    }
    let total_bytes = offer.files.iter().try_fold(0_u64, |total, file| {
        total.checked_add(file.size).ok_or({
            ReceiveError::Protocol(ProtocolError::Malformed("transfer size exceeds u64"))
        })
    })?;
    on_event(ReceiveEvent::TransferOffered {
        files: offer.files.len(),
        bytes: total_bytes,
    });

    let plan = prepare_files(root, &offer)?;
    let accept = TransferAccept {
        id: offer.id,
        resume_offsets: plan.iter().map(|file| file.resume_offset).collect(),
    };
    write_frame(&mut stream, FrameKind::Accept, &encode_accept(&accept)?)?;

    let mut received_total = 0_u64;
    for (expected_index, planned) in plan.iter().enumerate() {
        let start = expect_frame(&mut stream, FrameKind::FileStart)?;
        let (index, offset) = decode_file_start(&start)?;
        if index as usize != expected_index || offset != planned.resume_offset {
            return Err(ProtocolError::Malformed("file start does not match accept").into());
        }
        on_event(ReceiveEvent::FileStarted {
            path: planned.offer.path.as_path().to_path_buf(),
            size: planned.offer.size,
        });

        let mut partial = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&planned.partial)?;
        let mut hasher = hash_prefix(&mut partial, offset)?;
        partial.seek(SeekFrom::Start(offset))?;
        let mut position = offset;
        received_total += offset;
        on_event(ReceiveEvent::Progress {
            received: received_total,
            total: total_bytes,
        });

        while position < planned.offer.size {
            let frame = expect_frame(&mut stream, FrameKind::FileChunk)?;
            let (chunk_index, bytes) = decode_file_chunk(&frame)?;
            if chunk_index != index {
                return Err(ProtocolError::Malformed("file chunk index is out of sequence").into());
            }
            let remaining = planned.offer.size - position;
            if bytes.is_empty() || bytes.len() as u64 > remaining {
                return Err(ProtocolError::Malformed("file chunk exceeds offered size").into());
            }
            partial.write_all(bytes)?;
            hasher.update(bytes);
            position += bytes.len() as u64;
            received_total += bytes.len() as u64;
            on_event(ReceiveEvent::Progress {
                received: received_total,
                total: total_bytes,
            });
        }

        let end = expect_frame(&mut stream, FrameKind::FileEnd)?;
        let (end_index, sender_hash) = decode_file_end(&end)?;
        if end_index != index || sender_hash != planned.offer.hash {
            return Err(ProtocolError::Malformed("file end does not match offer").into());
        }
        partial.flush()?;
        partial.sync_all()?;
        let received_hash = *hasher.finalize().as_bytes();
        if received_hash != planned.offer.hash {
            partial.set_len(0)?;
            return Err(ReceiveError::HashMismatch(
                planned.offer.path.as_path().to_path_buf(),
            ));
        }
        drop(partial);
        fs::rename(&planned.partial, &planned.destination)?;
        on_event(ReceiveEvent::FileCompleted {
            path: planned.destination.clone(),
        });
    }

    let complete = expect_frame(&mut stream, FrameKind::Complete)?;
    let completed = decode_complete(&complete)?;
    if completed as usize != plan.len() {
        return Err(ProtocolError::Malformed("completion count does not match offer").into());
    }
    write_frame(
        &mut stream,
        FrameKind::Complete,
        &encode_complete(completed),
    )?;
    on_event(ReceiveEvent::Complete {
        files: plan.len(),
        bytes: total_bytes,
    });
    Ok(())
}

fn prepare_root(output: &Path) -> Result<PathBuf, ReceiveError> {
    fs::create_dir_all(output)?;
    let metadata = fs::symlink_metadata(output)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReceiveError::UnsafeDestination(format!(
            "receive root is not a real directory: {}",
            output.display()
        )));
    }
    Ok(output.canonicalize()?)
}

fn prepare_files(root: &Path, offer: &TransferOffer) -> Result<Vec<ReceiveFile>, ReceiveError> {
    let partial_root = root.join(".sdft-partials");
    ensure_real_directory(root, &partial_root)?;

    let mut plan = Vec::with_capacity(offer.files.len());
    let mut destinations = std::collections::HashSet::new();
    for file in &offer.files {
        let requested = file.offer_path_beneath(root);
        let parent = requested.parent().ok_or_else(|| {
            ReceiveError::UnsafeDestination("destination has no parent".to_owned())
        })?;
        ensure_real_directory(root, parent)?;
        let destination = unique_destination(&requested, &destinations)?;
        destinations.insert(destination.clone());

        let partial = partial_root.join(partial_name(file));
        if partial.exists() {
            let metadata = fs::symlink_metadata(&partial)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ReceiveError::UnsafeDestination(format!(
                    "partial path is not a regular file: {}",
                    partial.display()
                )));
            }
        }
        let mut resume_offset = partial.metadata().map_or(0, |metadata| metadata.len());
        if resume_offset > file.size {
            File::create(&partial)?.set_len(0)?;
            resume_offset = 0;
        }
        plan.push(ReceiveFile {
            offer: file.clone(),
            partial,
            destination,
            resume_offset,
        });
    }
    Ok(plan)
}

trait OfferPath {
    fn offer_path_beneath(&self, root: &Path) -> PathBuf;
}

impl OfferPath for FileOffer {
    fn offer_path_beneath(&self, root: &Path) -> PathBuf {
        self.path.beneath(root)
    }
}

fn ensure_real_directory(root: &Path, directory: &Path) -> Result<(), ReceiveError> {
    let relative = directory.strip_prefix(root).map_err(|_| {
        ReceiveError::UnsafeDestination(format!(
            "destination escapes receive root: {}",
            directory.display()
        ))
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(ReceiveError::UnsafeDestination(format!(
                    "destination component is not a real directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                let metadata = fs::symlink_metadata(&current)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ReceiveError::UnsafeDestination(format!(
                        "created destination component is unsafe: {}",
                        current.display()
                    )));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn unique_destination(
    requested: &Path,
    reserved: &std::collections::HashSet<PathBuf>,
) -> Result<PathBuf, ReceiveError> {
    if !requested.exists() && !reserved.contains(requested) {
        return Ok(requested.to_path_buf());
    }

    let parent = requested
        .parent()
        .ok_or_else(|| ReceiveError::UnsafeDestination("destination has no parent".to_owned()))?;
    let stem = requested
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            ReceiveError::UnsafeDestination("invalid destination filename".to_owned())
        })?;
    let extension = requested.extension().and_then(std::ffi::OsStr::to_str);
    for number in 1..=10_000 {
        let name = extension.map_or_else(
            || format!("{stem} ({number})"),
            |extension| format!("{stem} ({number}).{extension}"),
        );
        let candidate = parent.join(name);
        if !candidate.exists() && !reserved.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(ReceiveError::UnsafeDestination(format!(
        "could not find a free destination name for {}",
        requested.display()
    )))
}

fn partial_name(file: &FileOffer) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(file.path.as_path().to_string_lossy().as_bytes());
    hasher.update(&file.size.to_be_bytes());
    hasher.update(&file.hash);
    format!("{}.part", hasher.finalize().to_hex())
}

fn hash_prefix(file: &mut File, length: u64) -> Result<blake3::Hasher, ReceiveError> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = blake3::Hasher::new();
    let mut remaining = length;
    let mut buffer = vec![0_u8; 128 * 1024];
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| ReceiveError::Protocol(ProtocolError::Malformed("resume overflow")))?;
        let read = file.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(
                ProtocolError::Malformed("partial file is shorter than resume offset").into(),
            );
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(hasher)
}

fn expect_frame(stream: &mut TcpStream, expected: FrameKind) -> Result<Vec<u8>, ReceiveError> {
    let frame = read_frame(stream)?;
    if frame.kind != expected {
        return Err(ProtocolError::Malformed("peer sent an unexpected frame").into());
    }
    Ok(frame.payload)
}

fn expect_empty(stream: &mut TcpStream, expected: FrameKind) -> Result<(), ReceiveError> {
    let payload = expect_frame(stream, expected)?;
    if !payload.is_empty() {
        return Err(ProtocolError::Malformed("expected an empty frame").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::{SocketAddr, TcpListener};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::SafeRelativePath;
    use crate::protocol::FileOffer;
    use crate::sender::send_paths;

    use super::{partial_name, receive_once};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("sdft-{label}-{}-{timestamp}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn run_transfer(source: &Path, output: &Path) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        let output = output.to_path_buf();
        let receiver = thread::spawn(move || receive_once(&listener, &output, |_| {}));
        send_paths(address, &[source.to_path_buf()], |_| {}).unwrap();
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn transfers_and_verifies_a_file() {
        let fixture = TestDirectory::new("transfer");
        let source = fixture.path().join("source.bin");
        let output = fixture.path().join("received");
        let contents = vec![0x5a; 700_000];
        fs::write(&source, &contents).unwrap();

        run_transfer(&source, &output);

        assert_eq!(fs::read(output.join("source.bin")).unwrap(), contents);
    }

    #[test]
    fn resumes_an_existing_partial_file() {
        let fixture = TestDirectory::new("resume");
        let source = fixture.path().join("resume.bin");
        let output = fixture.path().join("received");
        let contents: Vec<u8> = (0_u32..800_000)
            .map(|value| u8::try_from(value % 251).unwrap())
            .collect();
        fs::write(&source, &contents).unwrap();
        let offer = FileOffer {
            path: SafeRelativePath::new(Path::new("resume.bin")).unwrap(),
            size: contents.len() as u64,
            hash: *blake3::hash(&contents).as_bytes(),
        };
        let partial_root = output.join(".sdft-partials");
        fs::create_dir_all(&partial_root).unwrap();
        let partial = partial_root.join(partial_name(&offer));
        fs::write(&partial, &contents[..333_333]).unwrap();

        run_transfer(&source, &output);

        assert_eq!(fs::read(output.join("resume.bin")).unwrap(), contents);
        assert!(!partial.exists());
    }

    #[test]
    fn keeps_both_when_a_destination_exists() {
        let fixture = TestDirectory::new("collision");
        let source = fixture.path().join("photo.jpg");
        let output = fixture.path().join("received");
        fs::create_dir_all(&output).unwrap();
        fs::write(&source, b"new photo").unwrap();
        fs::write(output.join("photo.jpg"), b"old photo").unwrap();

        run_transfer(&source, &output);

        assert_eq!(fs::read(output.join("photo.jpg")).unwrap(), b"old photo");
        assert_eq!(
            fs::read(output.join("photo (1).jpg")).unwrap(),
            b"new photo"
        );
    }
}
