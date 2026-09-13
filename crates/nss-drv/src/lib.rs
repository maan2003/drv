//! Thin glibc NSS adapter: one foreign-pointer boundary, all policy and I/O in safe Rust.
#![deny(unsafe_code)]
#[allow(unsafe_code)]
mod ffi;
mod safe;
