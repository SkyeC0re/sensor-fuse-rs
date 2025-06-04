//! Sensor core functionality and standard sensor core implementations.
//!

pub mod alloc;
// pub mod no_alloc;

use core::{
    future::Future,
    ops::{Deref, DerefMut},
};

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If it's not broken don't fix it.
pub(crate) const CLOSED_BIT: usize = 1;
pub(crate) const VERSION_BUMP: usize = 2;
pub(crate) const VERSION_MASK: usize = !CLOSED_BIT;
pub(crate) const VERSION_INIT: usize = usize::wrapping_sub(0, VERSION_BUMP);
