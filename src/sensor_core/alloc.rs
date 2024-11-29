pub struct SendTest<T: Send>(T);

use core::{
    future::Future,
    pin::Pin,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use core::{hint::spin_loop, mem::MaybeUninit};
use std::future::poll_fn;

use async_lock::{
    futures::{Read, Write},
    RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use either::Either;

use crate::ObservationStatus;

use super::{SensorCore, SensorCoreAsync, CLOSED_BIT, VERSION_BUMP, VERSION_MASK};

const INIT_BIT: usize = 1;
const DROP_BIT: usize = 2;
const PTR_MASK: usize = !(INIT_BIT | DROP_BIT);

// Ensure that WakerNodes cannot exist with addresses utilizing the lowest 2 bits.
#[repr(align(4))]
struct WakerNode {
    waker: MaybeUninit<Waker>,
    next: AtomicUsize,
}
struct WakerList {
    head: AtomicPtr<WakerNode>,
}

struct WaitFut<'a> {
    list: &'a WakerList,
    node: *mut WakerNode,
}

unsafe impl<'a> Send for WaitFut<'a> {}

impl<'a> Future for WaitFut<'a> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.node == null_mut() {
            self.node = Box::into_raw(Box::new(WakerNode {
                waker: MaybeUninit::new(cx.waker().clone()),
                next: AtomicUsize::new(0),
            }));

            let old_head = self.list.head.swap(self.node, Ordering::Release);

            unsafe {
                (*self.node)
                    .next
                    .store(old_head as usize | INIT_BIT, Ordering::Relaxed)
            };

            return Poll::Pending;
        }

        if unsafe { (*self.node).next.load(Ordering::Acquire) } & DROP_BIT > 0 {
            drop(unsafe { Box::from_raw(self.node) });
            self.node = null_mut();
            return Poll::Ready(());
        }

        return Poll::Pending;
    }
}

impl<'a> Drop for WaitFut<'a> {
    fn drop(&mut self) {
        if self.node == null_mut() {
            return;
        }

        if unsafe { (*self.node).next.fetch_or(DROP_BIT, Ordering::Acquire) } & DROP_BIT > 0 {
            drop(unsafe { Box::from_raw(self.node) });
        }
    }
}

impl WakerList {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
        }
    }

    fn wait(&self) -> WaitFut {
        WaitFut {
            list: self,
            node: null_mut(),
        }
    }

    fn wake_all(&self) {
        let mut list = self.head.swap(null_mut(), Ordering::Acquire);

        let mut state;
        while list != null_mut() {
            state = unsafe { (*list).next.load(Ordering::Relaxed) };
            while state & INIT_BIT == 0 {
                spin_loop();
                state = unsafe { (*list).next.load(Ordering::Relaxed) };
            }

            unsafe {
                if state & DROP_BIT > 0 {
                    (*list).waker.assume_init_drop();
                } else {
                    (*list).waker.assume_init_read().wake();

                    if (*list).next.fetch_or(DROP_BIT, Ordering::Release) & DROP_BIT == 0 {
                        list = (state & PTR_MASK) as *mut WakerNode;
                        continue;
                    }
                }
                drop(Box::from_raw(list));
            }
            list = (state & PTR_MASK) as *mut WakerNode;
        }
    }
}

/// Standard asyncronous sensor core.
pub struct AsyncCore<T> {
    version: AtomicUsize,
    lock: RwLock<T>,
    waiter_list: WakerList,
    writers: AtomicUsize,
}

impl<T> AsyncCore<T> {
    /// Create a new asyncronous sensor core.
    #[inline]
    pub const fn new(init: T) -> Self {
        Self {
            version: AtomicUsize::new(0),
            lock: RwLock::new(init),
            waiter_list: WakerList::new(),
            writers: AtomicUsize::new(0),
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

    type ReadGuard<'read> = RwLockReadGuard<'read, T> where Self: 'read;

    type WriteGuard<'write> = RwLockWriteGuard<'write, T> where Self: 'write;

    fn version(&self) -> usize {
        self.version.load(Ordering::Acquire)
    }

    fn mark_unseen(&self) {
        let _ = self.version.fetch_add(VERSION_BUMP, Ordering::Release);
        self.waiter_list.wake_all();
    }

    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.lock.try_read()
    }

    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.lock.try_write()
    }

    fn register_writer(&self) {
        let _ = self.writers.fetch_add(1, Ordering::Relaxed);
    }

    fn deregister_writer(&self) {
        if self.writers.fetch_sub(1, Ordering::Relaxed) == 1 {
            let _ = self.version.fetch_or(CLOSED_BIT, Ordering::Release);
        }
    }
}

impl<T> SensorCoreAsync for AsyncCore<T> {
    #[allow(refining_impl_trait)]
    fn read(&self) -> Read<'_, T> {
        self.lock.read()
    }

    #[allow(refining_impl_trait)]
    fn write(&self) -> Write<'_, T> {
        self.lock.write()
    }

    #[allow(refining_impl_trait)]
    fn wait_changed(
        &self,
        reference_version: usize,
    ) -> impl Future<Output = (usize, ObservationStatus)> + Send {
        let mut wait_fut = Either::Left(&self.waiter_list);
        let version = &self.version;
        poll_fn(move |cx| {
            let mut curr_version = version.load(Ordering::Acquire);
            let mut closed = curr_version & CLOSED_BIT > 0;
            if curr_version & VERSION_MASK != reference_version & VERSION_MASK || closed {
                return Poll::Ready((
                    curr_version,
                    ObservationStatus::new()
                        .modify_closed(closed)
                        .modify_success(!closed),
                ));
            }

            if let Either::Left(waiter_list) = wait_fut {
                let mut wait_fut_init = waiter_list.wait();
                // Result can be ignored, we know that this is always pending on the first poll which puts it in the waiterlist,
                // guaranteeing that any future update will wake this future. We need not do anything else with this future again, we
                // will guarantee that the version will be updated by the time the waker is called. There is therefore no need to ever poll it again.
                let _ = Pin::new(&mut wait_fut_init).poll(cx);
                wait_fut = Either::Right(wait_fut_init);

                // Do a second check on initial insertion to ensure we do not miss a version update.
                curr_version = version.load(Ordering::Acquire);
                closed = curr_version & CLOSED_BIT > 0;
                if curr_version & VERSION_MASK != reference_version & VERSION_MASK || closed {
                    return Poll::Ready((
                        curr_version,
                        ObservationStatus::new()
                            .modify_closed(closed)
                            .modify_success(!closed),
                    ));
                }
            }

            return Poll::Pending;
        })
    }

    async fn modify<M: FnOnce(&mut Self::Target) -> bool>(
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
