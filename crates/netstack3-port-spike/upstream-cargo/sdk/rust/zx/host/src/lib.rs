// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host value-type boundary for the Zircon API.

pub use zx_status::*;

pub mod sys {
    pub use zx_types::*;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_values_and_raw_type_are_exact() {
        let raw: sys::zx_status_t = Status::INVALID_ARGS.into_raw();
        assert_eq!(raw, zx_types::ZX_ERR_INVALID_ARGS);
        assert_eq!(Status::from_raw(raw), Status::INVALID_ARGS);
    }
}
