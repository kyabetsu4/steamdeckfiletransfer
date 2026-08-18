//! Shared protocol constants and filesystem-safety primitives.

pub mod protocol;
pub mod receiver;
pub mod sender;

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Prefix used to reject traffic meant for another protocol.
pub const PROTOCOL_MAGIC: [u8; 4] = *b"SDFT";

/// Increment only when peers cannot safely communicate with older releases.
pub const PROTOCOL_VERSION: u16 = 1;

/// Default TCP port used by the receiver.
pub const DEFAULT_PORT: u16 = 49_321;

/// A normalized transfer path that cannot escape a receiver-selected root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeRelativePath(PathBuf);

impl SafeRelativePath {
    /// Validates and normalizes a path supplied by a remote peer.
    ///
    /// Empty paths, absolute paths, platform prefixes, and parent traversal are
    /// rejected. `.` components are ignored.
    ///
    /// # Errors
    ///
    /// Returns [`UnsafePath`] when the input is empty or could address a path
    /// outside the receiver-selected root.
    pub fn new(path: &Path) -> Result<Self, UnsafePath> {
        let mut normalized = PathBuf::new();

        for component in path.components() {
            match component {
                Component::Normal(part) if !part.is_empty() => normalized.push(part),
                Component::CurDir => {}
                Component::Normal(_) => return Err(UnsafePath::EmptyComponent),
                Component::ParentDir => return Err(UnsafePath::ParentTraversal),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(UnsafePath::AbsoluteOrPrefixed);
                }
            }
        }

        if normalized.as_os_str().is_empty() {
            return Err(UnsafePath::Empty);
        }

        Ok(Self(normalized))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn beneath(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsafePath {
    Empty,
    EmptyComponent,
    ParentTraversal,
    AbsoluteOrPrefixed,
}

impl fmt::Display for UnsafePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "the path is empty",
            Self::EmptyComponent => "the path contains an empty component",
            Self::ParentTraversal => "the path contains parent traversal",
            Self::AbsoluteOrPrefixed => "the path is absolute or platform-prefixed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for UnsafePath {}

#[cfg(test)]
mod tests {
    use super::{SafeRelativePath, UnsafePath};
    use std::path::Path;

    #[test]
    fn accepts_a_normal_relative_path() {
        let path = SafeRelativePath::new(Path::new("games/screenshots/image.png"))
            .expect("a normal relative path should be accepted");
        assert_eq!(path.as_path(), Path::new("games/screenshots/image.png"));
    }

    #[test]
    fn rejects_parent_traversal() {
        let error = SafeRelativePath::new(Path::new("games/../../secret"))
            .expect_err("parent traversal must be rejected");
        assert_eq!(error, UnsafePath::ParentTraversal);
    }

    #[test]
    fn rejects_an_absolute_path() {
        let error = SafeRelativePath::new(Path::new("/etc/passwd"))
            .expect_err("absolute paths must be rejected");
        assert_eq!(error, UnsafePath::AbsoluteOrPrefixed);
    }

    #[test]
    fn rejects_an_empty_path() {
        let error = SafeRelativePath::new(Path::new("")).expect_err("empty paths must be rejected");
        assert_eq!(error, UnsafePath::Empty);
    }
}
