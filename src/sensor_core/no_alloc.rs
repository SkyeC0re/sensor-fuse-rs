use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::{cell::UnsafeCell, task::Waker};
use core::{
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};
use std::hint::unreachable_unchecked;
use std::ptr::null_mut;

use either::Either;

use crate::{
    ObservationStatus, SymResult, Version, STATUS_CHANGED_BIT, STATUS_CLOSED_BIT,
    STATUS_SUCCESS_BIT,
};

use super::{SensorCore, SensorCoreAsync, CLOSED_BIT, VERSION_BUMP};

const BIT_NODE_TYPE: usize = 1;
const BIT_READY: usize = 2;
const PTR_MASK: usize = !(BIT_NODE_TYPE | BIT_READY);

const NODE_TYPE_WRITER: usize = 1;

const MAX_READERS: usize = usize::MAX - 1;

#[repr(transparent)]
pub struct AsyncSingleCore<T>(UnsafeCell<Inner<T>>);

struct Inner<T> {
    version: Version,
    registered_writers: usize,
    // `usize::MAX` is writer active.
    reader_count: usize,
    // Fair Reader-Writer queue.
    // Combination of pointer to `Node` in higher bits and node type in lowest bit.
    queue_head: usize,
    queue_tail: usize,
    // Idle reads awaiting a version update.
    idle_reads_head: *mut Node,
    idle_reads_tail: *mut Node,

    waiters_head: *mut Node,

    // Ensure that `Inner` is not `Send` or `Sync`.
    _types: PhantomData<*mut Node>,
    data: T,
}

impl<T> Inner<T> {
    /// Either wakes the first set of queued readers or a single writer.
    /// Safety: should only be called with no active readers or writers.
    fn wake_next_set(&mut self) {
        if self.queue_head & BIT_NODE_TYPE == NODE_TYPE_WRITER {
            self.reader_count = usize::MAX;

            unsafe {
                let node = &mut *((self.queue_head & PTR_MASK) as *mut Node);
                self.queue_head = node.next;
                node.prev |= BIT_READY;
                node.waker.assume_init_read().wake();
            }

            // Ensure list coherence when the last element is removed.
            if self.queue_head == 0 {
                self.queue_tail = 0;
            }

            return;
        }

        self.reader_count = 0;

        loop {
            // No need to panic. Just leave the remaining readers for the next set.
            if (self.reader_count == MAX_READERS)
                || (self.queue_head & BIT_NODE_TYPE == NODE_TYPE_WRITER)
            {
                return;
            }

            self.reader_count += 1;

            unsafe {
                let node = &mut *((self.queue_head & PTR_MASK) as *mut Node);
                self.queue_head = node.next;
                node.prev |= BIT_READY;
                node.waker.assume_init_read().wake();
            }

            // Ensure list coherence when the last element is removed.
            if self.queue_head == 0 {
                self.queue_tail = 0;
                return;
            }
        }
    }
}

impl<T> AsyncSingleCore<T> {
    /// Forcefully acquire a read guard.
    ///
    /// # Safety
    ///
    /// Only safe to call once all writers have been dropped.
    #[inline(always)]
    const unsafe fn force_read(&self) -> ReadGuard<'_, T> {
        ReadGuard(&self.0)
    }
}

#[repr(align(4))]
struct Node {
    // Combination of pointer and node type.
    next: usize,
    // Combination of pointer and node type.
    prev: usize,
    waker: MaybeUninit<Waker>,
}

pub struct ReadGuard<'a, T>(
    // Safety: This should imply `!Send` and `!Sync`.
    &'a UnsafeCell<Inner<T>>,
);

impl<'a, T> Deref for ReadGuard<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.0.get()).data }
    }
}

impl<'a, T> Drop for ReadGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        let inner = unsafe { &mut (*self.0.get()) };

        if inner.reader_count == 1 {
            inner.wake_next_set();
            return;
        }

        inner.reader_count -= 1;
    }
}

pub struct WriteGuard<'a, T>(
    // Safety: This should imply `!Send` and `!Sync`.
    &'a UnsafeCell<Inner<T>>,
);

impl<'a, T> Deref for WriteGuard<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.0.get()).data }
    }
}

impl<'a, T> DerefMut for WriteGuard<'a, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut (*self.0.get()).data }
    }
}

impl<'a, T> Drop for WriteGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        unsafe { (*self.0.get()).wake_next_set() };
    }
}

pub struct ConditionalRead<'a, T> {
    node: Either<Version, Node>,
    // Safety: This should imply `!Send` and `!Sync`.
    core: &'a AsyncSingleCore<T>,
}

impl<'a, T> Future for ConditionalRead<'a, T> {
    type Output = (ReadGuard<'a, T>, Version);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: `condition` function is not moved.
        let s = unsafe { self.get_unchecked_mut() };
        let inner = unsafe { &mut *s.core.0.get() };

        if inner.version.closed_bit_set() {
            return Poll::Ready((unsafe { s.core.force_read() }, inner.version));
        }

        match &mut s.node {
            Either::Left(reference_version) => {
                if *reference_version == inner.version {
                    let node = Node {
                        next: 0,
                        prev: inner.idle_reads_tail as usize,
                        waker: MaybeUninit::new(cx.waker().clone()),
                    };

                    s.node = Either::Right(node);

                    // Get the pinned Node address.
                    let node_addr: *mut Node = match &mut s.node {
                        Either::Left(_) => unsafe { unreachable_unchecked() },
                        Either::Right(node) => node,
                    };

                    if inner.idle_reads_head == null_mut() {
                        inner.idle_reads_head = node_addr;
                    } else {
                        // Safety: A non-zero head implies a non-zero tail.
                        unsafe { (*inner.idle_reads_tail).next = node_addr as usize };
                    }

                    inner.idle_reads_tail = node_addr;
                    return Poll::Pending;
                }

                if let Some(read_guard) = s.core.try_read() {
                    return Poll::Ready((read_guard, inner.version));
                }

                let node = Node {
                    next: 0,
                    prev: inner.queue_tail,
                    waker: MaybeUninit::new(cx.waker().clone()),
                };

                s.node = Either::Right(node);
                // Get the pinned Node address.
                let node_addr: *mut Node = match &mut s.node {
                    Either::Left(_) => unsafe { unreachable_unchecked() },
                    Either::Right(node) => node,
                };

                if inner.queue_head == 0 {
                    inner.queue_head = node_addr as usize;
                } else {
                    // Safety: A non-zero head implies a non-zero tail.
                    unsafe {
                        (*((inner.queue_tail & PTR_MASK) as *mut Node)).next = node_addr as usize
                    };
                }

                inner.queue_tail = node_addr as usize;
            }
            Either::Right(node) => {
                if node.prev & BIT_READY != 0 {
                    return Poll::Ready((ReadGuard(&s.core.0), inner.version));
                }
            }
        }

        Poll::Pending
    }
}

impl<'a, T> Drop for ConditionalRead<'a, T> {
    fn drop(&mut self) {
        let node = match &mut self.node {
            Either::Left(_) => return,
            Either::Right(node) => node,
        };

        // Node has already been removed from the queue by a call to `fn@wake_next_set`, nothing to do.
        if node.prev & BIT_READY == BIT_READY {
            return;
        }

        unsafe {
            node.waker.assume_init_drop();
        }

        let inner = unsafe { &mut *self.core.0.get() };

        if node.prev >= PTR_MASK {
            // Node is not a head, connect `prev` to `next`.
            let prev_node = unsafe { &mut *((node.prev & PTR_MASK) as *mut Node) };
            prev_node.next = node.next;
        } else if (inner.queue_head & PTR_MASK) as *mut Node == node {
            // Node is the queue head.
            inner.queue_head = node.next;
        } else {
            // Node must be the idle readers head.
            inner.idle_reads_head = node.next as *mut Node;
        }

        if node.next >= PTR_MASK {
            // Node is not a tail, connect `next` to `prev`.
            let next_node = unsafe { &mut *((node.next & PTR_MASK) as *mut Node) };
            // Safety: This node was not ready, it is impossible for the next node to be ready and therefore have that state overwritten.
            next_node.prev = node.prev;
        } else if (inner.queue_tail & PTR_MASK) as *mut Node == node {
            // Node is the queue tail.
            inner.queue_tail = node.prev;
        } else {
            // Node must be the idle readers tail.
            inner.idle_reads_tail = node.prev as *mut Node;
        }
    }
}

pub struct Read<'a, T> {
    node: Option<Node>,
    // Safety: This should imply `!Send` and `!Sync`.
    core: &'a AsyncSingleCore<T>,
}

impl<'a, T> Future for Read<'a, T> {
    type Output = ReadGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = unsafe { self.get_unchecked_mut() };
        match &mut s.node {
            None => {
                let inner = unsafe { &mut *s.core.0.get() };

                if let Some(read_guard) = s.core.try_read() {
                    return Poll::Ready(read_guard);
                }

                let node = Node {
                    next: 0,
                    prev: inner.queue_tail,
                    waker: MaybeUninit::new(cx.waker().clone()),
                };

                s.node = Some(node);
                // Get the pinned Node address.
                let node_addr: *mut Node = match &mut s.node {
                    Some(v) => v,
                    None => unsafe { unreachable_unchecked() },
                };

                if inner.queue_head == 0 {
                    inner.queue_head = node_addr as usize;
                } else {
                    // Safety: A non-zero head implies a non-zero tail.
                    unsafe {
                        (*((inner.queue_tail & PTR_MASK) as *mut Node)).next = node_addr as usize
                    };
                }

                inner.queue_tail = node_addr as usize;
            }
            Some(node) => {
                if node.prev & BIT_READY == BIT_READY {
                    return Poll::Ready(ReadGuard(&s.core.0));
                }
            }
        }

        Poll::Pending
    }
}

impl<'a, T> Drop for Read<'a, T> {
    fn drop(&mut self) {
        let node = match &mut self.node {
            None => return,
            Some(node) => node,
        };

        // Node has already been removed from the queue by a call to `fn@wake_next_set`, nothing to do.
        if node.prev & BIT_READY == BIT_READY {
            return;
        }

        unsafe {
            node.waker.assume_init_drop();
        }

        let inner = unsafe { &mut *self.core.0.get() };

        if node.prev >= PTR_MASK {
            // Node is not a head, connect `prev` to `next`.
            let prev_node = unsafe { &mut *((node.prev & PTR_MASK) as *mut Node) };
            prev_node.next = node.next;
        } else {
            // Node is the queue head.
            inner.queue_head = node.next;
        }

        if node.next >= PTR_MASK {
            // Node is not a tail, connect `next` to `prev`.
            let next_node = unsafe { &mut *((node.next & PTR_MASK) as *mut Node) };
            // Safety: This node was not ready, it is impossible for the next node to be ready and therefore have that state overwritten.
            next_node.prev = node.prev;
        } else {
            // Node is the queue tail.
            inner.queue_tail = node.prev;
        }
    }
}

pub struct Write<'a, T> {
    node: Option<Node>,
    // Safety: This should imply `!Send` and `!Sync`.
    core: &'a AsyncSingleCore<T>,
}

impl<'a, T> Future for Write<'a, T> {
    type Output = WriteGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = self.get_mut();

        if let Some(node) = &mut s.node {
            return if node.prev & BIT_READY == BIT_READY {
                Poll::Ready(WriteGuard(&s.core.0))
            } else {
                Poll::Pending
            };
        }

        if let Some(write_guard) = unsafe { s.core.try_write() } {
            return Poll::Ready(write_guard);
        }

        let inner = unsafe { &mut *s.core.0.get() };

        let node = Node {
            next: 0,
            prev: inner.queue_tail,
            waker: MaybeUninit::new(cx.waker().clone()),
        };

        s.node = Some(node);
        // Get the pinned Node address.
        let node_addr: *mut Node = unsafe { s.node.as_mut().unwrap_unchecked() };

        if inner.queue_head == 0 {
            inner.queue_head = node_addr as usize;
        }

        inner.queue_tail = node_addr as usize;

        Poll::Pending
    }
}

impl<'a, T> Drop for Write<'a, T> {
    fn drop(&mut self) {
        let node = match &mut self.node {
            None => return,
            Some(node) => node,
        };

        // Node has already been removed from the queue by a call to `fn@wake_next_set`, nothing to do.
        if node.prev & BIT_READY == BIT_READY {
            return;
        }

        unsafe {
            node.waker.assume_init_drop();
        }

        let inner = unsafe { &mut *self.core.0.get() };

        if node.prev >= PTR_MASK {
            // Node is not a head, connect `prev` to `next` whilst ensuring ready bit is propogated correctly.
            let prev_node = unsafe { &mut *((node.prev & PTR_MASK) as *mut Node) };
            prev_node.next = node.next;
        } else {
            // Node is the queue head.
            inner.queue_head = node.next;
        };

        if node.next >= PTR_MASK {
            // Node is not a tail, connect `next` to `prev`.
            let next_node = unsafe { &mut *((node.next & PTR_MASK) as *mut Node) };
            // Safety: This node was not ready, it is impossible for the next node to be ready and therefore have that state overwritten.
            next_node.prev = node.prev;
        } else {
            // Node is the queue tail.
            inner.queue_tail = node.prev;
        };
    }
}

pub struct WaitChanged<'a, T> {
    node: Either<Version, Node>,
    // Safety: This should imply `!Send` and `!Sync`.
    core: &'a AsyncSingleCore<T>,
}

impl<'a, T> Future for WaitChanged<'a, T> {
    type Output = Version;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = self.get_mut();
        let inner = unsafe { &mut *s.core.0.get() };

        if inner.version.closed_bit_set() {
            return Poll::Ready(inner.version);
        }

        match &mut s.node {
            Either::Left(reference_version) => {
                if *reference_version == inner.version {
                    let node = Node {
                        next: inner.waiters_head as usize,
                        prev: 0,
                        waker: MaybeUninit::new(cx.waker().clone()),
                    };

                    s.node = Either::Right(node);

                    // Get the pinned Node address.
                    let node_addr: *mut Node = match &mut s.node {
                        Either::Left(_) => unsafe { unreachable_unchecked() },
                        Either::Right(node) => node,
                    };

                    if inner.waiters_head != null_mut() {
                        unsafe { (*inner.waiters_head).prev = node_addr as usize };
                    }

                    inner.waiters_head = node_addr;
                    return Poll::Pending;
                }
            }
            Either::Right(node) => {
                if node.prev & BIT_READY == 0 {
                    return Poll::Pending;
                }
            }
        }

        Poll::Ready(inner.version)
    }
}

impl<T> SensorCore for AsyncSingleCore<T> {
    type Target = T;

    type ReadGuard<'read>
        = ReadGuard<'read, T>
    where
        Self: 'read;

    type WriteGuard<'write>
        = WriteGuard<'write, T>
    where
        Self: 'write;

    #[inline(always)]
    unsafe fn register_writer(&self) {
        (*self.0.get()).registered_writers += 1
    }
    #[inline(always)]
    unsafe fn deregister_writer(&self) {
        (*self.0.get()).registered_writers -= 1
    }
    #[inline(always)]
    fn version(&self) -> Version {
        unsafe { (*self.0.get()).version }
    }
    #[inline(always)]
    fn mark_unseen(&self) {
        let inner = unsafe { &mut *self.0.get() };
        inner.version.increment();

        // Clear idle reader list.
        if inner.idle_reads_head != null_mut() {
            if inner.queue_tail == 0 {
                while inner.idle_reads_head != null_mut() && inner.reader_count < MAX_READERS {
                    let node = unsafe { &mut *inner.idle_reads_head };
                    inner.idle_reads_head = (node.next & PTR_MASK) as *mut Node;
                    inner.reader_count += 1;
                    node.prev |= BIT_READY;
                    unsafe { node.waker.assume_init_read().wake() };
                }
                // Insert the remainder that exceeds `MAX_READERS` into the queue to be awoken as the next set.
                if inner.idle_reads_head != null_mut() {
                    unsafe { (*inner.idle_reads_head).prev = 0 };
                    inner.queue_head = inner.idle_reads_head as usize;
                    inner.queue_tail = inner.idle_reads_tail as usize;
                }
            } else {
                unsafe {
                    (*inner.idle_reads_head).prev = inner.queue_tail;
                    (*((inner.queue_tail & PTR_MASK) as *mut Node)).next =
                        inner.idle_reads_head as usize;
                }
                inner.queue_tail = inner.idle_reads_tail as usize;
            }

            inner.idle_reads_head = null_mut();
            inner.idle_reads_tail = null_mut();
        }

        // Clear waiters.
        while inner.waiters_head != null_mut() {
            let node = unsafe { &mut *inner.waiters_head };
            inner.waiters_head = node.next as *mut Node;
            node.next |= BIT_READY;
            unsafe { node.waker.assume_init_read().wake() };
        }
    }
    #[inline]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        let inner = unsafe { &mut (*self.0.get()) };

        if inner.queue_head == 0 && inner.reader_count < MAX_READERS {
            inner.reader_count += 1;
            return Some(ReadGuard(&self.0));
        }
        None
    }

    /// Attempts to acquire a write guard.
    ///
    /// # Safety
    ///
    /// It is undefined behaviour to call this without a registered writer.
    #[inline]
    unsafe fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        let inner = &mut (*self.0.get());

        if inner.queue_head == 0 {
            inner.reader_count = usize::MAX;
            return Some(WriteGuard(&self.0));
        }

        None
    }
}

impl<T> SensorCoreAsync for AsyncSingleCore<T> {
    #[allow(refining_impl_trait)]
    fn read(&self) -> Read<'_, T> {
        Read {
            node: None,
            core: self,
        }
    }

    #[allow(refining_impl_trait)]
    fn write(&self) -> Write<'_, T> {
        Write {
            node: None,
            core: self,
        }
    }

    fn wait_changed(&self, reference_version: Version) -> impl Future<Output = Version> {
        WaitChanged {
            node: Either::Left(reference_version),
            core: self,
        }
    }

    async fn wait_for<C: FnMut(&Self::Target) -> bool>(
        &self,
        mut condition: C,
        mut reference_version: Version,
    ) -> (Self::ReadGuard<'_>, Version) {
        loop {
            let res = ConditionalRead {
                node: Either::Left(reference_version),
                core: self,
            }
            .await;
            if !res.1.closed_bit_set() {
                if !condition(&res.0) {
                    reference_version = res.1;
                    continue;
                }
            }

            return res;
        }
    }
}
