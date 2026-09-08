// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host compatibility boundary for Fuchsia Inspect convenience helpers.

#[macro_export]
macro_rules! inspect_insert {
    ($($tokens:tt)*) => {{}};
}

#[macro_export]
macro_rules! inspect_log {
    ($($tokens:tt)*) => {{}};
}

pub mod log {
    use std::marker::PhantomData;

    pub struct InspectBytes<'a>(pub &'a [u8]);
    pub struct InspectUintArray<'a>(pub &'a [u8]);
    pub struct InspectListClosure<'a, T, F>(pub &'a [T], pub F);

    impl<'a, T, F> InspectListClosure<'a, T, F> {
        pub fn marker(&self) -> PhantomData<T> {
            PhantomData
        }
    }
}

pub mod nodes {
    use fuchsia_inspect::Node;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    pub struct BoundedListNode {
        _node: Node,
        _capacity: usize,
    }

    impl BoundedListNode {
        pub fn new(node: Node, capacity: usize) -> Self {
            Self {
                _node: node,
                _capacity: capacity,
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct MonotonicTimeProperty(Arc<Mutex<zx::MonotonicInstant>>);

    impl MonotonicTimeProperty {
        pub fn set(&self, value: zx::MonotonicInstant) {
            *self.0.lock().expect("monotonic property lock poisoned") = value;
        }
        pub fn set_at(&self, value: zx::MonotonicInstant) {
            self.set(value);
        }
        pub fn get(&self) -> zx::MonotonicInstant {
            *self.0.lock().expect("monotonic property lock poisoned")
        }
    }

    pub trait NodeTimeExt {
        fn create_time_at(
            &self,
            name: impl AsRef<str>,
            value: zx::MonotonicInstant,
        ) -> MonotonicTimeProperty;
    }

    impl NodeTimeExt for Node {
        fn create_time_at(
            &self,
            _name: impl AsRef<str>,
            value: zx::MonotonicInstant,
        ) -> MonotonicTimeProperty {
            MonotonicTimeProperty(Arc::new(Mutex::new(value)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::nodes::*;

    #[test]
    fn monotonic_property_preserves_initial_value() {
        let node = fuchsia_inspect::Node::default();
        let value = zx::MonotonicInstant::from_nanos(42);
        assert_eq!(node.create_time_at("time", value).get(), value);
        let property = node.create_time_at("updated", value);
        property.set_at(zx::MonotonicInstant::from_nanos(84));
        assert_eq!(property.get(), zx::MonotonicInstant::from_nanos(84));
    }
}
