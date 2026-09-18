use std::fmt;

/// Sanitized errors intentionally exclude request URLs, bodies, credentials and bank text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    Configuration,
    Authentication { status: Option<u16> },
    Http { status: u16, indeterminate: bool },
    Transport { indeterminate: bool },
    Decode { indeterminate: bool },
}
impl Error {
    /// A mutation may have reached the bank. Reconcile through GET before retrying.
    pub fn is_indeterminate(&self) -> bool {
        matches!(
            self,
            Self::Http {
                indeterminate: true,
                ..
            } | Self::Transport {
                indeterminate: true
            } | Self::Decode {
                indeterminate: true
            }
        )
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Authentication { status } => *status,
            _ => None,
        }
    }
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration => f.write_str("invalid C6 client configuration"),
            Self::Authentication { .. } => f.write_str("C6 authentication failed"),
            Self::Http { status, .. } => write!(f, "C6 request returned HTTP {status}"),
            Self::Transport { .. } => f.write_str("C6 transport failed"),
            Self::Decode { .. } => f.write_str("invalid C6 response"),
        }
    }
}
impl std::error::Error for Error {}
