use std::fmt;

/// Sanitized errors intentionally exclude request URLs, bodies, credentials and bank text.
#[derive(Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    Configuration,
    Authentication {
        status: Option<u16>,
        retry_after: Option<String>,
    },
    Http {
        status: u16,
        retry_after: Option<String>,
        indeterminate: bool,
    },
    Transport {
        indeterminate: bool,
    },
    Decode {
        indeterminate: bool,
    },
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
            Self::Authentication { status, .. } => *status,
            _ => None,
        }
    }
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Http { status: 404, .. })
    }
    /// Returns the native `Retry-After` value without interpreting a duration or HTTP date.
    pub fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Authentication { retry_after, .. } | Self::Http { retry_after, .. } => {
                retry_after.as_deref()
            }
            _ => None,
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration => f.write_str("Configuration"),
            Self::Authentication { status, .. } => f
                .debug_struct("Authentication")
                .field("status", status)
                .finish(),
            Self::Http {
                status,
                indeterminate,
                ..
            } => f
                .debug_struct("Http")
                .field("status", status)
                .field("indeterminate", indeterminate)
                .finish(),
            Self::Transport { indeterminate } => f
                .debug_struct("Transport")
                .field("indeterminate", indeterminate)
                .finish(),
            Self::Decode { indeterminate } => f
                .debug_struct("Decode")
                .field("indeterminate", indeterminate)
                .finish(),
        }
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

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn debug_redacts_retry_after_for_http_and_authentication_errors() {
        let http = Error::Http {
            status: 429,
            retry_after: Some("private-bank-header".into()),
            indeterminate: false,
        };
        let authentication = Error::Authentication {
            status: Some(429),
            retry_after: Some("private-bank-header".into()),
        };

        for error in [http, authentication] {
            let debug = format!("{error:?}");
            assert!(debug.contains("429"));
            assert!(!debug.contains("private-bank-header"));
        }
    }
}
