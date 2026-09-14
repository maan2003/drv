// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory host boundary for the Fuchsia Inspect property API.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct Node;

impl Node {
    pub fn create_child(&self, _name: impl AsRef<str>) -> Self {
        Self
    }
    pub fn create_uint(&self, _name: impl AsRef<str>, value: u64) -> UintProperty {
        UintProperty(Arc::new(AtomicU64::new(value)))
    }
    pub fn create_int(&self, _name: impl AsRef<str>, value: i64) -> IntProperty {
        IntProperty(Arc::new(AtomicI64::new(value)))
    }
    pub fn create_bool(&self, _name: impl AsRef<str>, value: bool) -> BoolProperty {
        BoolProperty(Arc::new(AtomicBool::new(value)))
    }
    pub fn create_string(&self, _name: impl AsRef<str>, value: impl AsRef<str>) -> StringProperty {
        StringProperty(Arc::new(Mutex::new(value.as_ref().to_owned())))
    }
    pub fn create_bytes(&self, _name: impl AsRef<str>, value: impl AsRef<[u8]>) -> BytesProperty {
        BytesProperty(Arc::new(Mutex::new(value.as_ref().to_vec())))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Inspector {
    root: Node,
}

impl Inspector {
    pub fn root(&self) -> &Node {
        &self.root
    }
    pub fn copy_vmo(&self) -> Option<fidl::Vmo> {
        Some(Vec::new())
    }
}

pub trait Property {}
pub trait NumericProperty: Property {}

#[derive(Clone, Debug)]
pub struct UintProperty(Arc<AtomicU64>);
impl Property for UintProperty {}
impl NumericProperty for UintProperty {}
impl UintProperty {
    pub fn set(&self, value: u64) {
        self.0.store(value, Ordering::Relaxed);
    }
    pub fn add(&self, value: u64) {
        self.0.fetch_add(value, Ordering::Relaxed);
    }
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct IntProperty(Arc<AtomicI64>);
impl Property for IntProperty {}
impl NumericProperty for IntProperty {}
impl IntProperty {
    pub fn set(&self, value: i64) {
        self.0.store(value, Ordering::Relaxed);
    }
    pub fn add(&self, value: i64) {
        self.0.fetch_add(value, Ordering::Relaxed);
    }
    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct BoolProperty(Arc<AtomicBool>);
impl Property for BoolProperty {}
impl BoolProperty {
    pub fn set(&self, value: bool) {
        self.0.store(value, Ordering::Relaxed);
    }
    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct StringProperty(Arc<Mutex<String>>);
impl Property for StringProperty {}
impl StringProperty {
    pub fn set(&self, value: &str) {
        *self.0.lock().unwrap() = value.to_owned();
    }
    pub fn get(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Clone, Debug)]
pub struct BytesProperty(Arc<Mutex<Vec<u8>>>);
impl Property for BytesProperty {}
impl BytesProperty {
    pub fn set(&self, value: &[u8]) {
        *self.0.lock().unwrap() = value.to_vec();
    }
    pub fn get(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_retain_host_values() {
        let node = Inspector::default().root().create_child("sme");
        let counter = node.create_uint("discarded", 1);
        counter.add(2);
        assert_eq!(counter.get(), 3);
        let status = node.create_string("status", "idle");
        status.set("connected");
        assert_eq!(status.get(), "connected");
    }

    #[test]
    fn snapshot_boundary_is_available() {
        assert_eq!(Inspector::default().copy_vmo(), Some(Vec::new()));
    }
}
