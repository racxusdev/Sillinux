use std::fmt;

/// Errors produced while parsing a `.studio3` container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file is shorter than the fixed outer header.
    TooShort { need: usize, got: usize },
    /// The file does not start with `silhouette05;`.
    BadMagic,
    /// The three bytes after the magic are not NUL.
    BadMagicPadding,
    /// No nested `silhouette05;` document was found after the outer header.
    NoNestedDocument,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooShort { need, got } => {
                write!(f, "file too short: need at least {need} bytes, got {got}")
            }
            Error::BadMagic => write!(f, "not a .studio3 file: missing `silhouette05;` magic"),
            Error::BadMagicPadding => write!(f, "malformed header: magic padding is not NUL"),
            Error::NoNestedDocument => {
                write!(
                    f,
                    "no nested `silhouette05;` document after the outer header"
                )
            }
        }
    }
}

impl std::error::Error for Error {}
