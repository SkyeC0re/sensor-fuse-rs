use core::sync::atomic::{AtomicUsize, Ordering};

use async_lock::{
    RwLock, RwLockReadGuard, RwLockWriteGuard,
    futures::{Read, Write},
};
use event_listener::{Event, IntoNotification, listener};

use crate::{
    RefWrapper, SensorObserve, SensorObserveAsync, SensorWrite, SensorWriteAsync, ShareStrategy,
    SymResult, Wrapper,
};

use super::{CLOSED_BIT, VERSION_BUMP, VERSION_INIT};

#[repr(transparent)]
pub struct Writer<T, R: ShareStrategy<Target = Core<T>>> {
    core: R,
}

impl<T> Writer<T, Wrapper<Core<T>>> {
    #[inline(always)]
    pub const fn new_const(init: T) -> Self {
        Self {
            core: Wrapper(Core::new(init)),
        }
    }

    #[inline(always)]
    pub const fn observe_ref_const(&self) -> Observer<T, RefWrapper<Core<T>>> {
        Observer {
            core: RefWrapper(&self.core.0),
            version: VERSION_INIT,
        }
    }

    #[inline(always)]
    pub const fn clone_ref_const(&self) -> Writer<T, RefWrapper<Core<T>>> {
        Writer {
            core: RefWrapper(&self.core.0),
        }
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> Writer<T, R> {
    pub fn clone_ref(&self) -> Writer<T, RefWrapper<Core<T>>> {
        Writer {
            core: RefWrapper(&self.core),
        }
    }

    #[inline(always)]
    pub fn observe_ref(&self) -> Observer<T, RefWrapper<Core<T>>> {
        Observer {
            core: RefWrapper(&self.core),
            version: VERSION_INIT,
        }
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> Writer<T, R>
where
    R: Clone,
{
    #[inline(always)]
    pub fn observe(&self) -> Observer<T, R> {
        Observer {
            core: self.core.clone(),
            version: VERSION_INIT,
        }
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> Clone for Writer<T, R>
where
    R: Clone,
{
    fn clone(&self) -> Self {
        if R::PERMANENT {
            let writers = self.core.writers.fetch_add(1, Ordering::Relaxed);
            if writers > usize::MAX >> 1 {
                panic!("Too many writers");
            }
        }
        Self {
            core: self.core.clone(),
        }
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> Drop for Writer<T, R> {
    fn drop(&mut self) {
        if R::PERMANENT {
            if self.core.writers.fetch_sub(1, Ordering::Relaxed) == 1 {
                let _ = self
                    .core
                    .version_data
                    .v
                    .fetch_or(CLOSED_BIT, Ordering::Relaxed);

                let _ = self.core.version_data.updated.notify(1.additional());
            }
        }
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> Writer<T, R>
where
    R: From<Core<T>>,
{
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self {
            core: R::from(Core::new(init)),
        }
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> From<T> for Writer<T, R>
where
    R: From<Core<T>>,
{
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
    version: usize,
}

impl<T, R: ShareStrategy<Target = Core<T>>> SensorObserve for Observer<T, R> {
    type Target = T;

    type ReadGuard<'read>
        = RwLockReadGuard<'read, T>
    where
        Self: 'read;

    #[inline]
    fn mark_seen(&mut self) {
        self.version = self.core.version_data.v.load(Ordering::Relaxed);
    }

    #[inline]
    fn mark_unseen(&mut self) {
        self.version = self
            .core
            .version_data
            .v
            .load(Ordering::Relaxed)
            .wrapping_sub(VERSION_BUMP);
    }

    #[inline]
    fn has_changed(&self) -> bool {
        self.version != self.core.version_data.v.load(Ordering::Relaxed)
    }

    #[inline]
    fn is_closed(&self) -> bool {
        self.core.version_data.v.load(Ordering::Relaxed) & CLOSED_BIT != 0
    }
}

impl<T, R: ShareStrategy<Target = Core<T>>> Observer<T, R> {
    async fn wait_changed_inner(&self) -> SymResult<()> {
        let version_data = &self.core.version_data;
        let mut curr_version = version_data.v.load(Ordering::Relaxed);
        while (curr_version & CLOSED_BIT == 0) && curr_version == self.version {
            listener!(version_data.updated => version_changed);
            version_changed.await;
            let _ = version_data.updated.notify(1.additional());
            curr_version = version_data.v.load(Ordering::Relaxed);
        }

        return match curr_version & CLOSED_BIT == 0 {
            true => Ok(()),
            false => Err(()),
        };
    }
}

#[allow(refining_impl_trait)]
impl<T, R: ShareStrategy<Target = Core<T>>> SensorObserveAsync for Observer<T, R> {
    fn read<'a>(&'a mut self) -> Read<'a, T> {
        self.core.lock.read()
    }

    async fn wait_changed(&mut self) -> SymResult<()> {
        self.wait_changed_inner().await
    }

    async fn wait_for<F>(&mut self, mut condition: F) -> SymResult<Self::ReadGuard<'_>>
    where
        F: for<'b> FnMut(&'b Self::Target) -> bool,
    {
        let mut res = match self.is_closed() {
            true => Err(()),
            false => Ok(()),
        };

        loop {
            let guard = self.core.lock.read().await;
            if res.is_err() {
                return Err(guard);
            }
            if condition(&guard) {
                return Ok(guard);
            }
            drop(guard);
            res = self.wait_changed_inner().await;
        }
    }

    async fn wait_for_next<'a, F: FnMut(&Self::Target) -> bool>(
        &'a mut self,
        mut condition: F,
    ) -> SymResult<Self::ReadGuard<'a>> {
        loop {
            let res = self.wait_changed_inner().await;
            let guard = self.core.lock.read().await;
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
pub struct Core<T> {
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

#[cfg(test)]
mod tests {
    use crate::{SensorObserveAsync, SensorWriteAsync, Wrapper};

    use super::Writer;

    #[repr(transparent)]
    struct IsSend<S: Send>(S);

    /// We prove that all futures relating to `AsyncCore` are inherently `Send`.
    #[test]
    fn send_proofs() {
        let writer = Writer::<_, Wrapper<_>>::new_const(0);
        let mut reader = writer.observe_ref();
        let mut writer = writer.clone_ref();

        let _ = IsSend(writer.write());
        let _ = IsSend(writer.modify(|_| true));
        let _ = IsSend(reader.read());
        let _ = IsSend(reader.wait_for(|_| true));
        let _ = IsSend(reader.wait_for_next(|_| true));
        let _ = IsSend(reader.wait_changed());
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
