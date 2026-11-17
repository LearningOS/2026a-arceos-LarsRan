//! Collections in ArceOS.
//!
//! The [`alloc`] crate ships the tree/linked collections such as [`BTreeMap`],
//! [`BTreeSet`], [`VecDeque`] and [`BinaryHeap`], but it provides no hash-based
//! collections. [`HashMap`] and [`HashSet`] are therefore re-exported from the
//! [`hashbrown`] crate, which is the same implementation that `std` uses
//! internally.

pub use alloc::collections::*;
pub use hashbrown::{HashMap, HashSet};
