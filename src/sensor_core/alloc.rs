use core::{
    future::Future,
    pin::Pin,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use core::{hint::spin_loop, mem::MaybeUninit};
use std::{
    cell::UnsafeCell,
    future::poll_fn,
    mem,
    ops::{Deref, DerefMut},
    sync::{atomic::AtomicU8, Arc},
};

use async_lock::{
    futures::{Read, Write},
    RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use either::Either;
use event_listener::{listener, Event, IntoNotification};
use futures::FutureExt;

use crate::{SensorWrite, SensorWriteAsync, SymResult, Version};

use super::{SensorCore, SensorCoreAsync, CLOSED_BIT, VERSION_BUMP};

macro_rules! miri_log {
    ($($arg:tt)*) => {
        #[cfg(feature = "dev-miri-logs")]
        println!($($arg)*)
    };
}

struct VersionData {
    v: AtomicUsize,
    updated: Event,
}

/// Standard asyncronous sensor core.
pub struct AsyncCore<T> {
    lock: RwLock<T>,
    writers: AtomicUsize,
    version_data: VersionData,
}

impl<T> AsyncCore<T> {
    /// Create a new asyncronous sensor core.
    #[inline]
    pub const fn new(init: T) -> Self {
        Self {
            lock: RwLock::new(init),
            writers: AtomicUsize::new(1),
            version_data: VersionData {
                v: AtomicUsize::new(0),
                updated: Event::new(),
            },
        }
    }
}

impl<T> From<T> for AsyncCore<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> SensorCore for AsyncCore<T> {
    type Target = T;

    type ReadGuard<'read>
        = RwLockReadGuard<'read, T>
    where
        Self: 'read;

    type WriteGuard<'write>
        = RwLockWriteGuard<'write, T>
    where
        Self: 'write;

    #[inline(always)]
    fn version(&self) -> Version {
        Version(self.version_data.v.load(Ordering::Acquire))
    }

    #[inline]
    fn mark_unseen(&self) {
        let _ = self
            .version_data
            .v
            .fetch_add(VERSION_BUMP, Ordering::Relaxed);
        let _ = self.version_data.updated.notify(1.additional());
    }

    #[inline]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.lock.try_read()
    }

    #[inline]
    unsafe fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.lock.try_write()
    }

    #[inline]
    unsafe fn register_writer(&self) {
        let _ = self.writers.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    unsafe fn deregister_writer(&self) {
        if self.writers.fetch_sub(1, Ordering::Relaxed) == 1 {
            let _ = self.version_data.v.fetch_or(CLOSED_BIT, Ordering::Relaxed);
            let _ = self.version_data.updated.notify(1.additional());
        }
    }
}

impl<T> SensorCoreAsync for AsyncCore<T> {
    #[allow(refining_impl_trait)]
    #[inline(always)]
    fn read(&self) -> Read<'_, T> {
        self.lock.read()
    }

    #[allow(refining_impl_trait)]
    #[inline(always)]
    fn write(&self) -> Write<'_, T> {
        self.lock.write()
    }

    #[allow(refining_impl_trait)]
    #[inline]
    fn wait_changed(&self, reference_version: Version) -> impl Future<Output = Version> + Send {
        let version_data = &self.version_data;
        async move {
            let mut curr_version = Version(version_data.v.load(Ordering::Relaxed));
            while !curr_version.closed_bit_set() && curr_version == reference_version {
                listener!(version_data.updated => version_changed);
                version_changed.await;
                let _ = version_data.updated.notify(1.additional());
                curr_version = Version(version_data.v.load(Ordering::Relaxed));
            }

            return curr_version;
        }
    }

    #[inline]
    async unsafe fn modify<M: FnOnce(&mut Self::Target) -> bool>(
        &self,
        modifier: M,
    ) -> (Self::WriteGuard<'_>, bool) {
        let mut guard = self.write().await;
        let modified = modifier(&mut guard);
        if modified {
            self.mark_unseen();
        }
        (guard, modified)
    }
}

#[cfg(test)]
mod tests {
    use crate::{sensor_core::SensorCoreAsync, Version};

    use super::AsyncCore;

    #[repr(transparent)]
    struct IsSend<S: Send>(S);

    /// We prove that all futures relating to `AsyncCore` are inherently `Send`.
    #[test]
    fn send_proofs() {
        let core = AsyncCore::new(0);

        let _ = IsSend(core.read());
        let _ = IsSend(core.write());
        let _ = IsSend(unsafe { core.modify(|_| true) });
        let _ = IsSend(core.wait_changed(Version(0)));
        let _ = IsSend(core.wait_for(|_| true, Version(0)));
        let _ = IsSend(core.write());
    }
}

#[cfg(test)]
mod test {
    use std::{hint::spin_loop, sync::Arc, thread, time::Duration};

    use futures::executor::block_on;

    use crate::{SensorObserveAsync, SensorWriter};

    use super::AsyncCore;

    // use super::MyWriter;

    #[test]
    fn test_this() {
        let mut writer = SensorWriter::<AsyncCore<_>, Arc<AsyncCore<_>>>::from_value(5);

        let mut threads = Vec::new();
        for i in 0..2 {
            let mut reader = writer.spawn_observer();

            let handle = thread::spawn(move || {
                let mut prev = 100;
                while prev < 80000 {
                    prev = *block_on(reader.wait_for(|x| *x > prev)).unwrap();

                    for i in 0..50 {
                        spin_loop();
                    }
                }
            });

            threads.push(handle);
        }

        for i in -1000..=80000 {
            let mut guard = block_on(writer.write());

            println!("i: {}", i);

            *guard = i;
            drop(guard);
            writer.mark_all_unseen();
        }

        for t in threads {
            t.join().unwrap();
        }
    }
}
