// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host error boundary for generic FIDL responder helpers.

use std::fmt;

/// Host-owned snapshot bytes used at the Inspect/FIDL boundary.
pub type Vmo = Vec<u8>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidHeader,
    TransportUnavailable,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeader => f.write_str("Invalid header for a FIDL buffer."),
            Self::TransportUnavailable => f.write_str("FIDL transport is unavailable on the host."),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_header_is_reportable() {
        assert_eq!(
            Error::InvalidHeader.to_string(),
            "Invalid header for a FIDL buffer."
        );
        assert_eq!(
            Error::TransportUnavailable.to_string(),
            "FIDL transport is unavailable on the host."
        );
    }
}
