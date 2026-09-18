#![doc = include_str!("../README.md")]

//! Async C6 Bank Pix client with shared OAuth tokens and mutual TLS.
//!
//! Monetary values are exact decimal strings. Mutations are never retried by the
//! default transport. If [`Error::is_indeterminate`] is true, reconcile by reading
//! the same transaction ID before retrying; never generate a replacement ID.
mod client;
pub mod dto;
mod error;
pub use client::{Client, ClientBuilder, Environment};
pub use dto::*;
pub use error::Error;
