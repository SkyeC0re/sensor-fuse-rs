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

use crate::{
    SensorObserve, SensorObserveAsync, SensorWrite, SensorWriteAsync, ShareStrategy, SymResult,
    Version, Wrapper,
};

use super::{SensorCore, CLOSED_BIT, VERSION_BUMP};

macro_rules! miri_log {
    ($($arg:tt)*) => {
        #[cfg(feature = "dev-miri-logs")]
        println!($($arg)*)
    };
}

#[repr(transparent)]
pub struct Writer<T, R: ShareStrategy<Target = Core<T>>> {
    core: R,
}

impl<T, R: ShareStrategy<Target = Core<T>>> Writer<T, R> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self {
            core: R::init(Core::new(init)),
        }
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> From<T> for Writer<T, R> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> SensorWrite for Writer<T, R> {
    type Target = T;

    type WriteGuard<'a>
        = RwLockWriteGuard<'a, T>
    where
        Self: 'a;

    #[inline(always)]
    fn notify_all(&self) {
        let _ = self
            .core
            .version_data
            .v
            .fetch_add(VERSION_BUMP, Ordering::Relaxed);
        let _ = self.core.version_data.updated.notify(1.additional());
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.core.lock.try_write()
    }
}

#[allow(refining_impl_trait)]
impl<T, R: ShareStrategy<Target = Core<T>>> SensorWriteAsync for Writer<T, R> {
    #[inline(always)]
    fn write(&mut self) -> Write<'_, T> {
        self.core.lock.write()
    }

    #[inline]
    async fn modify<F: FnOnce(&mut Self::Target) -> bool>(&mut self, f: F) {
        let mut guard = self.core.lock.write().await;
        if f(&mut guard) {
            self.notify_all();
        }
    }

    #[inline]
    async fn update(&mut self, value: Self::Target) {
        self.modify(|v| {
            *v = value;
            true
        })
        .await;
    }
}

#[derive(Clone, Copy)]
pub struct Observer<T, R: ShareStrategy<Target = Core<T>>> {
    core: R,
    version: Version,
}

impl<T, R: ShareStrategy<Target = Core<T>>> SensorObserve for Observer<T, R> {
    type Target = T;

    type ReadGuard<'read>
        = RwLockReadGuard<'read, T>
    where
        Self: 'read;

    #[inline]
    fn mark_seen(&mut self) {
        self.version.0 = self.core.version_data.v.load(Ordering::Relaxed);
    }

    #[inline]
    fn mark_unseen(&mut self) {
        self.version.0 = self
            .core
            .version_data
            .v
            .load(Ordering::Relaxed)
            .wrapping_sub(VERSION_BUMP);
    }

    #[inline]
    fn has_changed(&self) -> bool {
        self.version.0 != self.core.version_data.v.load(Ordering::Relaxed)
    }

    #[inline]
    fn is_closed(&self) -> bool {
        self.core.version_data.v.load(Ordering::Relaxed) & CLOSED_BIT != 0
    }
}

#[allow(refining_impl_trait)]
impl<T, R: ShareStrategy<Target = Core<T>>> SensorObserveAsync for Observer<T, R> {
    #[inline]
    fn read(&self) -> Read<'_, T> {
        self.core.lock.read()
    }

    async fn wait_until_changed(&self) -> SymResult<()> {
        let version_data = &self.core.version_data;
        let mut curr_version = Version(version_data.v.load(Ordering::Relaxed));
        while !curr_version.closed_bit_set() && curr_version == self.version {
            listener!(version_data.updated => version_changed);
            version_changed.await;
            let _ = version_data.updated.notify(1.additional());
            curr_version = Version(version_data.v.load(Ordering::Relaxed));
        }

        return match curr_version.closed_bit_set() {
            true => Err(()),
            false => Ok(()),
        };
    }

    async fn wait_for<F: FnMut(&Self::Target) -> bool>(
        &mut self,
        mut condition: F,
    ) -> SymResult<Self::ReadGuard<'_>> {
        let mut res = match self.is_closed() {
            true => Err(()),
            false => Ok(()),
        };

        loop {
            let guard = self.read().await;
            if res.is_err() {
                return Err(guard);
            }
            if condition(&guard) {
                return Ok(guard);
            }
            res = self.wait_until_changed().await;
        }
    }

    async fn wait_for_next<F: FnMut(&Self::Target) -> bool>(
        &mut self,
        mut condition: F,
    ) -> SymResult<Self::ReadGuard<'_>> {
        loop {
            let res = self.wait_until_changed().await;

            let guard = self.read().await;
            if res.is_err() {
                return Err(guard);
            }

            if condition(&guard) {
                return Ok(guard);
            }
        }
    }
}

struct VersionData {
    v: AtomicUsize,
    updated: Event,
}

/// Standard asyncronous sensor core.
struct Core<T> {
    lock: RwLock<T>,
    writers: AtomicUsize,
    version_data: VersionData,
}

impl<T> Core<T> {
    /// Create a new asyncronous sensor core.
    #[inline(always)]
    const fn new(init: T) -> Self {
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

impl<T> From<T> for Core<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use crate::{sensor_core::SensorCoreAsync, SensorWriteAsync, Version, Wrapper};

    use super::{Core, Writer};

    #[repr(transparent)]
    struct IsSend<S: Send>(S);

    /// We prove that all futures relating to `AsyncCore` are inherently `Send`.
    #[test]
    fn send_proofs() {
        let mut writer = Writer::<_, Wrapper<_>>::new(0);

        let _ = IsSend(writer.write());
        let _ = IsSend(writer.modify(|_| true));
    }
}

// #[cfg(test)]
// mod test {
//     use std::{hint::spin_loop, sync::Arc, thread, time::Duration};

//     use futures::executor::block_on;

//     use crate::SensorObserveAsync;

//     use super::Core;

//     // use super::MyWriter;

//     #[test]
//     fn test_this() {
//         let mut writer = SensorWriter::<Core<_>, Arc<Core<_>>>::from_value(5);

//         let mut threads = Vec::new();
//         for i in 0..2 {
//             let mut reader = writer.spawn_observer();

//             let handle = thread::spawn(move || {
//                 let mut prev = 100;
//                 while prev < 80000 {
//                     prev = *block_on(reader.wait_for(|x| *x > prev)).unwrap();

//                     for i in 0..50 {
//                         spin_loop();
//                     }
//                 }
//             });

//             threads.push(handle);
//         }

//         for i in -1000..=80000 {
//             let mut guard = block_on(writer.write());

//             println!("i: {}", i);

//             *guard = i;
//             drop(guard);
//             writer.mark_all_unseen();
//         }

//         for t in threads {
//             t.join().unwrap();
//         }
//     }
// }
