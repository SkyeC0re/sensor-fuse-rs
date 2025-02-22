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
    marker::PhantomData,
    mem,
    num::{NonZero, NonZeroUsize},
    ops::{BitXor, Deref, DerefMut},
    ptr::NonNull,
    sync::atomic::AtomicU8,
};

use async_lock::{
    futures::{Read, Write},
    RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use either::Either;

use crate::{SymResult, Version};

use super::{no_alloc::WaitChanged, SensorCore, SensorCoreAsync, CLOSED_BIT, VERSION_BUMP};

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

    #[inline]
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
    #[inline]
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
    #[inline(always)]
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
        }
    }

    #[inline]
    fn wait(&self) -> WaitFut {
        WaitFut {
            list: self,
            node: null_mut(),
        }
    }

    #[inline]
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
            writers: AtomicUsize::new(1),
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
        Version(self.version.load(Ordering::Acquire))
    }

    #[inline]
    fn mark_unseen(&self) {
        let _ = self.version.fetch_add(VERSION_BUMP, Ordering::Release);
        self.waiter_list.wake_all();
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
            let _ = self.version.fetch_or(CLOSED_BIT, Ordering::Release);
            self.waiter_list.wake_all();
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
        let mut wait_fut = Either::Left(&self.waiter_list);
        let version = &self.version;
        poll_fn(move |cx| {
            let mut curr_version = Version(version.load(Ordering::Acquire));
            // We can safely ignore applying the version mask to the versions. A closed state can never be reverted and
            // as such if the closed bit is set on either `curr_version` or `reference_version` then `closed = true`.
            if curr_version.closed_bit_set() || curr_version != reference_version {
                return Poll::Ready(curr_version);
            }

            if let Either::Left(waiter_list) = wait_fut {
                let mut wait_fut_init = waiter_list.wait();
                // Result can be ignored, we know that this is always pending on the first poll which puts it in the waiterlist,
                // guaranteeing that any future update will wake this future. We need not do anything else with this future again, we
                // will guarantee that the version will be updated by the time the waker is called. There is therefore no need to ever poll it again.
                let _ = Pin::new(&mut wait_fut_init).poll(cx);
                wait_fut = Either::Right(wait_fut_init);

                // Do a second check on initial insertion to ensure we do not miss a version update.
                curr_version = Version(version.load(Ordering::Acquire));
                if curr_version.closed_bit_set() || curr_version != reference_version {
                    return Poll::Ready(curr_version);
                }
            }

            return Poll::Pending;
        })
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

// const Self::STATE_COMPLETE_BIT: u8 = 8;
// const Self::STATE_CANCEL_BIT: u8 = 4;
// const Self::STATE_DROP_BIT: u8 = 2;
// const Self::STATE_LOCK_BIT: u8 = 1;

#[repr(align(4))]
struct Node {
    waker: MaybeUninit<Waker>,
    // PTR | IS_LAST_BIT | TYPE_BIT
    next: AtomicUsize,
    // ... | COMPLETE_BIT | CANCEL_BIT | DROP_BIT | LOCK_BIT
    state: AtomicU8,
}

impl Node {
    const STATE_LOCK_BIT: u8 = 0b1;
    const STATE_DROP_BIT: u8 = 0b10;
    const STATE_CANCEL_BIT: u8 = 0b100;
    const STATE_COMPLETE_BIT: u8 = 0b1000;

    const NEXT_DATA_MASK: usize = 0b11;
    // Whether the node in the Read-Write is a write node.
    const NEXT_IS_WRITER_BIT: usize = 0b1;
    // Sentinal bit used for various purposes depending on what context the node is being used in.
    const NEXT_SENTINEL_BIT: usize = 0b10;
    const NEXT_PTR_MASK: usize = !Self::NEXT_DATA_MASK;
    const WAIT_CHANGED_TAIL: usize = 0b1;

    /// Attempts to recycle an old allocation or discard it an create a new allocation if that fails.
    /// If `None` is returned for the node, the waker was reset inside the queue, otherwise exlusive ownership is guaranteed
    /// over the (potentially newly allocated) Node.
    ///
    /// # Safety
    ///
    /// After this function returns with a mutable reference, it is up to the caller to ensure that the non-atomic modifications
    /// made to the node by this function as part of the reset process is appropriately released.
    ///
    /// It can be safely assumed that `None` can only ever be returned if `allow_in_queue_reset` is true.
    #[inline(always)]
    unsafe fn reuse_or_realloc(
        node: *mut Self,
        waker: Waker,
        allow_in_queue_reset: bool,
    ) -> Option<&'static mut Self> {
        let old_node = &mut *node;

        // Write free initial check for completion.
        let mut state = old_node.state.load(Ordering::Acquire);
        if state & Self::STATE_COMPLETE_BIT != 0 {
            // Free to re-use mutably. We are the sole owners.
            Node::assume_exclusive_reset(node, waker);
            return Some(&mut *node);
        }

        state = Self::lock(node);

        if state & Self::STATE_COMPLETE_BIT != 0 {
            // Free to re-use mutably. We are the sole owners.
            Node::assume_exclusive_reset(node, waker);
            return Some(&mut *node);
        }

        debug_assert_eq!(state, Self::STATE_CANCEL_BIT);

        if allow_in_queue_reset {
            // Piggy back off of the fact that the node is still in an appropriate queue.
            let _ = (*node).waker.write(waker);
            old_node.state.store(0, Ordering::Release);

            return None;
        }

        // No attempts at recycling the allocation succeeded. Dump it and acquire a new one.
        old_node
            .state
            .store(Self::STATE_DROP_BIT, Ordering::Release);

        let node = Box::new(Node {
            waker: MaybeUninit::new(waker),
            next: AtomicUsize::new(0),
            state: AtomicU8::new(Node::STATE_COMPLETE_BIT),
        });

        return Some(unsafe { &mut *Box::into_raw(node) });
    }

    /// Reset the node's values for a new future that is to be added to a queue.
    ///
    /// # Safety
    ///
    /// Behaviour is undefined if node is not in a completed (i.e. exclusive) state.
    #[inline(always)]
    unsafe fn assume_exclusive_reset(node: *mut Self, waker: Waker) {
        let node = &mut *node;
        let _ = node.waker.write(waker);
        *node.next.get_mut() = 0;
        *node.state.get_mut() = 0;
    }

    /// Cancels and drops the waker if the node has not been completed yet.
    ///
    /// Returns true if our cancellation was succesful. In this case the cancellation was announced before
    /// the completion could occur, otherwise the completion has occured with the implications which follows
    /// for it.
    ///
    /// # Safety
    ///
    /// It is undefined behaviour to call this twice on the same node without resetting it.
    #[inline(always)]
    unsafe fn cancel(node: *mut Self) -> bool {
        let node = &mut *node;

        let mut state = node.state.load(Ordering::Acquire);
        debug_assert_eq!(state & Self::STATE_CANCEL_BIT, 0);

        if state & (Self::STATE_COMPLETE_BIT) != 0 {
            // We have already been completed. Nothing to do.
            return false;
        }

        // A perceived lock state does not guarantee that we are about to be completed (as is the case for a write node).
        // Wait until we have a lock to guarantee our state.
        loop {
            while state & Self::STATE_LOCK_BIT != 0 {
                spin_loop();
                state = node.state.load(Ordering::Relaxed);
            }

            state = node.state.fetch_or(Self::STATE_LOCK_BIT, Ordering::Acquire);
            if state & Self::STATE_LOCK_BIT == 0 {
                break;
            }
        }

        if state & Self::STATE_COMPLETE_BIT != 0 {
            // We have already been completed. Nothing to do.
            return false;
        }

        // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
        // of dropping the waker.
        let waker = node.waker.assume_init_read();
        node.state.store(
            state ^ (Self::STATE_LOCK_BIT | Self::STATE_CANCEL_BIT),
            Ordering::Release,
        );
        drop(waker);

        true
    }

    /// Wake the next element in the wait changed queue, skipping over cancelled nodes and appropriately dropping
    /// drop requested nodes.
    ///
    /// # Safety
    ///
    /// It is undefined behaviour to call this before a node is completed (or being completed) or to call this twice for
    /// the same node for the same completion.
    /// After this call completes it is undefined behaviour to access the node again.
    #[inline(always)]
    unsafe fn assume_wait_changed_wake(mut node: *mut Self) {
        let mut next_node;
        loop {
            next_node = Node::assume_wait_changed_next_node(node);

            // Responsibility was either shifted successfully or we have reached the end of the queue.
            if Node::complete_and_wake(node, false) || next_node.is_null() {
                return;
            }

            node = next_node;
        }
    }

    /// Find the next node in the list. Assumes that the node is uncompleted and in the wait changed queue.
    ///
    /// # Safety
    ///
    /// This function should only be called on nodes that are in queue, and for which the caller either is the sole
    /// arbiter of the node's completion or can guarantee that the node has been cancelled before this call and has
    /// not been reset.
    #[inline(always)]
    unsafe fn assume_wait_changed_next_node(node: *mut Self) -> *mut Self {
        let mut next_ptr;
        loop {
            next_ptr = (*node).next.load(Ordering::Acquire);

            if next_ptr > 0 {
                return (next_ptr & Self::NEXT_PTR_MASK) as *mut _;
            }

            spin_loop();
        }
    }

    /// Attempt to complete wake this node if possible, and returns true if it either was successfully completed and awoken (i.e not previously cancelled or dropped), when `retired_only=false`
    /// or if it would have been successfully completed and awoken when `retired_only=true`.
    ///
    /// # Safety
    ///
    /// Should only be called on a node that is in a queue and has not been marked as completed (i.e. not previously cancelled or dropped).
    /// After this call completes it is undefined behaviour to access the node again, except for the case where `retired_only=true` and true is returned.
    #[inline(always)]
    unsafe fn complete_and_wake(node: *mut Self, retired_only: bool) -> bool {
        let mut state;
        loop {
            state = (*node)
                .state
                .fetch_or(Self::STATE_LOCK_BIT, Ordering::AcqRel);

            if state & Self::STATE_LOCK_BIT == 0 {
                break;
            }

            spin_loop();
        }

        if state == Self::STATE_DROP_BIT {
            drop(Box::from_raw(node));
            return false;
        }

        if state == Self::STATE_CANCEL_BIT {
            (*node)
                .state
                .store(state | Self::STATE_COMPLETE_BIT, Ordering::Release);
            return false;
        }

        if retired_only {
            (*node).state.store(state, Ordering::Release);
            return true;
        }

        debug_assert_eq!(state, 0);

        // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
        // of waking.
        let waker = (*node).waker.assume_init_read();
        (*node)
            .state
            .store(state | Self::STATE_COMPLETE_BIT, Ordering::Release);
        waker.wake();

        true
    }

    /// Locks the given node and returns the state just before the lock bit was set.
    /// # Safety
    ///
    /// Should only be called on nodes that are guaranteed to not get dropped.
    #[inline(always)]
    unsafe fn lock(node: *mut Self) -> u8 {
        let mut state = (*node).state.load(Ordering::Relaxed);

        // A perceived lock state does not guarantee that we are about to be completed (as is the case for a write node).
        // Wait until we have a lock to guarantee our state.
        loop {
            while state & Self::STATE_LOCK_BIT != 0 {
                spin_loop();
                state = (*node).state.load(Ordering::Relaxed);
            }

            state = (*node)
                .state
                .fetch_or(Self::STATE_LOCK_BIT, Ordering::Acquire);

            if state & Self::STATE_LOCK_BIT == 0 {
                return state;
            }
        }
    }
}

struct MySensorCore<T> {
    queue_head: AtomicUsize,
    queue_tail: AtomicUsize,
    idle_reads_head: AtomicPtr<Node>,
    idle_reads_tail: AtomicPtr<Node>,
    change_waiters_head: AtomicUsize,
    num_writers: AtomicUsize,
    writes_queued: AtomicUsize,
    readers: AtomicUsize,
    version: AtomicUsize,
    data: UnsafeCell<T>,
}

impl<T> MySensorCore<T> {
    #[inline(always)]
    fn version(&self) -> Version {
        // Relaxed could work?
        Version(self.version.load(Ordering::Acquire))
    }

    #[inline(always)]
    fn wake_waiters(&self) {
        // Swap the tail sentinal value in, taking effective ownership of the current queue and allowing
        // the next queue to start populating.
        // `Ordering::AcqRel` is not needed here, the sentinal indicator is sufficient.
        let node = self
            .change_waiters_head
            .swap(Node::WAIT_CHANGED_TAIL, Ordering::Acquire);

        if node == Node::WAIT_CHANGED_TAIL {
            return;
        }

        unsafe { Node::assume_wait_changed_wake(node as *mut _) };
    }

    #[inline(always)]
    fn try_read(&self) -> Option<ReadGuard<T>> {
        if self.writes_queued.load(Ordering::Relaxed) != 0 {
            return None;
        }

        self.try_prioritized_read()
    }

    /// Get a prioritized read lock.
    #[inline(always)]
    fn try_prioritized_read(&self) -> Option<ReadGuard<T>> {
        let mut readers = self.readers.load(Ordering::Relaxed);
        while readers < usize::MAX - 1 {
            match self.readers.compare_exchange_weak(
                readers,
                readers + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(ReadGuard { core: self }),
                Err(e) => readers = e,
            }

            spin_loop();
        }

        None
    }

    #[inline(always)]
    fn try_write(&self) -> Option<WriteGuard<T>> {
        if self.writes_queued.load(Ordering::Relaxed) != 0 {
            return None;
        }

        self.try_prioritized_write()
    }

    /// Get a prioritized read lock.
    #[inline(always)]
    fn try_prioritized_write(&self) -> Option<WriteGuard<T>> {
        if self
            .readers
            .compare_exchange(0, usize::MAX, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }

        Some(WriteGuard { core: self })
    }

    /// # Safety
    ///
    /// The caller must have a readguard. `node_ptr` must point to an actual node in the Read-Write queue.
    #[inline(always)]
    unsafe fn wake_next_reader(&self, mut node_ptr: usize) {
        let node = (node_ptr & Node::NEXT_PTR_MASK) as *mut Node;

        // We already have a readguard, this can only fail because we have reached the maximum amount of readers.
        // In this case, leave the current node in the queue and come back to it later.
        let readguard = match self.try_prioritized_read() {
            Some(v) => v,
            None => {
                self.queue_head.store(node_ptr, Ordering::Relaxed);
                return;
            }
        };

        loop {
            let state = Node::lock(node);

            if state & (Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT) != 0 {
                debug_assert_eq!(state & !(Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT), 0);

                let next_node_ptr = self.try_reset_queue(node_ptr);

                if state == Node::STATE_DROP_BIT {
                    drop(Box::from_raw(node));
                } else {
                    (*node)
                        .state
                        .store(state | Node::STATE_COMPLETE_BIT, Ordering::Release);
                }

                match next_node_ptr {
                    Some(next_node_ptr) => {
                        node_ptr = next_node_ptr;
                        continue;
                    }
                    // Nothing to do, list is cleared.
                    None => return,
                }
            }

            debug_assert_eq!(state, 0);

            // Uncancelled write detected. Stop waking here.
            if node_ptr & Node::NEXT_DATA_MASK != 0 {
                // Relaxed ordering is sufficient here, nothing was changed and we do not require any other data.
                (*node).state.store(0, Ordering::Relaxed);
                return;
            }

            // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
            // of waking.
            let waker = (*node).waker.assume_init_read();
            (*node)
                .state
                .store(Node::STATE_COMPLETE_BIT, Ordering::Release);
            waker.wake();

            // Pass the readguard to the awoken node.
            mem::forget(readguard);
            return;
        }
    }

    /// # Safety
    ///
    /// ???
    #[inline(always)]
    unsafe fn wake_next_in_queue(&self, holds_write_guard: bool) {
        let mut node_ptr = self.queue_head.load(Ordering::Acquire);

        if node_ptr == 0 || node_ptr & Node::NEXT_SENTINEL_BIT != 0 {
            // Either the queue is empty or someone else has taken the responsibility to wake the next element.

            debug_assert!(!holds_write_guard || Node::NEXT_SENTINEL_BIT == 0);
            return;
        }

        if self
            .queue_head
            .compare_exchange(
                node_ptr,
                node_ptr & Node::NEXT_SENTINEL_BIT,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return;
        }

        let node = (node_ptr & Node::NEXT_PTR_MASK) as *mut Node;

        loop {
            let state = Node::lock(node);

            if state & (Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT) != 0 {
                debug_assert_eq!(state & !(Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT), 0);

                let next_node_ptr = self.try_reset_queue(node_ptr);

                if state == Node::STATE_DROP_BIT {
                    drop(Box::from_raw(node));
                } else {
                    (*node)
                        .state
                        .store(state | Node::STATE_COMPLETE_BIT, Ordering::Release);
                }

                match next_node_ptr {
                    Some(next_node_ptr) => {
                        node_ptr = next_node_ptr;
                        continue;
                    }
                    // Nothing to do, list is cleared.
                    None => return,
                }
            }

            debug_assert_eq!(state, 0);

            if node_ptr & Node::NEXT_DATA_MASK != 0 {
                if !holds_write_guard {
                    match self.try_write() {
                        Some(write_guard) => {
                            mem::forget(write_guard);
                        }
                        None => {
                            // Relaxed ordering is sufficient here, nothing was changed and we don't access `next_node_ptr`.
                            (*node).state.store(0, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            } else {
                if holds_write_guard {
                    self.readers.store(1, Ordering::Relaxed);
                } else {
                    match self.try_read() {
                        Some(read_guard) => {
                            mem::forget(read_guard);
                        }
                        None => {
                            // Relaxed ordering is sufficient here, nothing was changed and we don't access `next_node_ptr`.
                            (*node).state.store(0, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            };

            // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
            // of waking.
            let waker = (*node).waker.assume_init_read();
            (*node)
                .state
                .store(Node::STATE_COMPLETE_BIT, Ordering::Release);
            waker.wake();

            return;
        }
    }

    /// Try to reset the Read-Write queue given non-zero typed node that is currently part of the queue.
    ///
    /// # Safety
    ///
    /// `possible_tail_node` should point to a valid node inside the the Read-Write queue.
    #[inline(always)]
    unsafe fn try_reset_queue(&self, possible_tail_node: usize) -> Option<usize> {
        let node = (possible_tail_node & Node::NEXT_PTR_MASK) as *mut Node;

        // Ensure that the next node's data is populated before any access to it.
        let mut next = (*node).next.load(Ordering::Acquire);
        if next != 0 {
            return Some(next);
        }

        if self
            .queue_tail
            .compare_exchange(possible_tail_node, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            // Ensure that when another thread reads the queue head, that when it observes a zeroed head, that a non-zero tail can only
            // exist because a new node is in the process of being attached and will soon update the queue head and that that tail
            // does not still contain `possible_tail_node`.
            self.queue_head.store(0, Ordering::Release);
            return None;
        }

        // A new node has attached to the tail, we cannot reset the queue.

        loop {
            // Ensure that the next node's data is populated before any access to it.
            next = (*node).next.load(Ordering::Acquire);
            if next != 0 {
                return Some(next);
            }

            spin_loop();
        }
    }
}

pub struct ReadGuard<'a, T> {
    core: &'a MySensorCore<T>,
}

impl<'a, T> Deref for ReadGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.core.data.get() }
    }
}

impl<'a, T> Drop for ReadGuard<'a, T> {
    fn drop(&mut self) {
        let remaining = self.core.readers.fetch_sub(1, Ordering::Relaxed);

        debug_assert!(remaining != 0);

        if remaining != 1 {
            return;
        }

        unsafe { self.core.wake_next_in_queue(false) };
    }
}

pub struct WriteGuard<'a, T> {
    core: &'a MySensorCore<T>,
}

impl<'a, T> Deref for WriteGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.core.data.get() }
    }
}

impl<'a, T> DerefMut for WriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.core.data.get() }
    }
}

impl<'a, T> Drop for WriteGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.core.wake_next_in_queue(true) };
    }
}

const SA_TYPE_WRITE: usize = 0;
const SA_TYPE_READ: usize = 1;
const SA_TYPE_IDLE_READ: usize = 2;
const SA_TYPE_WAIT_CHANGED: usize = 3;
const SA_MASK_TYPE: usize = 3;
const SA_MASK_PTR: usize = !SA_MASK_TYPE;

struct MyObserver<T, R: Deref<Target = MySensorCore<T>>> {
    core: R,
    data: ObserverData,
}

struct ObserverData {
    static_area: usize,
    version: Version,
}

impl<T, R: Deref<Target = MySensorCore<T>>> MyObserver<T, R> {
    pub fn wait_changed(&mut self) -> MyWaitChangedFut<T> {
        MyWaitChangedFut {
            core: &self.core,
            observer_data: &mut self.data,
            init: false,
        }
    }
}

struct MyReadFut<'a, T> {
    observer_data: &'a mut ObserverData,
    core: &'a MySensorCore<T>,
    init: bool,
}

impl<'a, T> Future for MyReadFut<'a, T> {
    type Output = SymResult<ReadGuard<'a, T>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = (self.observer_data.static_area & SA_MASK_PTR) as *mut Node;
        if self.init {
            if unsafe { (*node).state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0 } {
                return Poll::Pending;
            }

            // Whoever woke us has generated a readguard for us.
            let guard = ReadGuard { core: self.core };

            unsafe {
                if let Some(next_ptr) = self.core.try_reset_queue(node as usize) {
                    self.core.wake_next_reader(next_ptr);
                }
            }

            self.init = false;
            let version = self.core.version();
            self.observer_data.version = version;
            return Poll::Ready(match version.closed_bit_set() {
                true => Err(guard),
                false => Ok(guard),
            });
        }

        if let Some(guard) = self.core.try_read() {
            let version = self.core.version();
            return Poll::Ready(match version.closed_bit_set() {
                true => Err(guard),
                false => Ok(guard),
            });
        }

        let node_type = self.observer_data.static_area & SA_MASK_TYPE;
        let new_static_area = if let Some(node) = unsafe {
            Node::reuse_or_realloc(
                node,
                cx.waker().clone(),
                node_type == SA_TYPE_READ || node_type == SA_TYPE_WRITE,
            )
        } {
            let new_static_area: *mut Node = node;
            let old_tail = self
                .core
                .queue_tail
                // `AcqRel` needed here to ensure `reuse_or_realloc` invariants for both this node and the old tail.
                .swap(new_static_area as usize, Ordering::AcqRel);

            if old_tail == 0 {
                self.core
                    .queue_head
                    .store(new_static_area as usize, Ordering::Release);

                // We are the new queue head, see if we can awake immediately, so as to not stall if we just missed a waking of readers.
                if let Some(guard) = self.core.try_prioritized_read() {
                    match self.core.queue_head.compare_exchange(
                        new_static_area as usize,
                        new_static_area as usize | Node::NEXT_SENTINEL_BIT,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => unsafe {
                            // We have exlusive access to our node again, as we won the race to set the sentinel bit. We immediately complete ourselves.
                            node.waker.assume_init_drop();
                            *node.state.get_mut() = Node::STATE_COMPLETE_BIT;
                        },
                        Err(_) => {
                            // We are not responsible for waking but have been or are about to be awoken (the only function that could have set the sentinel bit is `wake_next_in_queue`).
                            // Wait until we are awoken via this mechanism, so as to guarantee that the node will completed and reusable.
                            while node.state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0
                            {
                                spin_loop();
                            }
                        }
                    }

                    unsafe {
                        // We are now responsible for waking.
                        if let Some(next_node) = self.core.try_reset_queue(new_static_area as usize)
                        {
                            self.core.wake_next_reader(next_node);
                        }

                        // Safety: We again have exlusive access to our node.
                        self.observer_data.static_area = new_static_area as usize | SA_TYPE_READ;

                        let version = self.core.version();
                        self.observer_data.version = version;
                        return Poll::Ready(match version.closed_bit_set() {
                            true => Err(guard),
                            false => Ok(guard),
                        });
                    }
                }
            } else {
                unsafe {
                    (*((old_tail & Node::NEXT_PTR_MASK) as *mut Node))
                        .next
                        .store(new_static_area as usize, Ordering::Release)
                };
            }

            new_static_area
        } else {
            node
        };

        self.observer_data.static_area = new_static_area as usize | SA_TYPE_READ;
        self.init = true;

        Poll::Pending
    }
}

impl<'a, T> Drop for MyReadFut<'a, T> {
    fn drop(&mut self) {
        if !self.init {
            return;
        }

        let node = (self.observer_data.static_area & SA_MASK_PTR) as *mut Node;

        if unsafe { Node::cancel(node) } {
            return;
        }

        // We have a readguard and are responsible for waking the next read future as well.

        unsafe {
            if let Some(next_ptr) = self.core.try_reset_queue(node as usize) {
                self.core.wake_next_reader(next_ptr);
                drop(ReadGuard { core: self.core });
            }
        }
    }
}

struct MyWaitChangedFut<'a, T> {
    observer_data: &'a mut ObserverData,
    core: &'a MySensorCore<T>,
    init: bool,
}

impl<'a, T> Future for MyWaitChangedFut<'a, T> {
    type Output = SymResult<Version>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = (self.observer_data.static_area & SA_MASK_PTR) as *mut Node;
        if self.init {
            if unsafe { (*node).state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0 } {
                return Poll::Pending;
            }

            let version = self.core.version();
            if self.observer_data.version != version {
                // Do not rely on `Drop` trait to wake the next element. It may not be instantaneous.
                unsafe {
                    if !Node::cancel(node) {
                        let next_node = Node::assume_wait_changed_next_node(node);
                        if next_node != null_mut() {
                            Node::assume_wait_changed_wake(node);
                        }
                    }
                }

                self.init = false;
                return Poll::Ready(version.as_result());
            }
        } else {
            let version = self.core.version();
            if self.observer_data.version != version {
                return Poll::Ready(version.as_result());
            }
        }

        // The version was unchanged. If `init == true` then we were not spuriosly awoken, which can only happen if
        // we reused a canceled node in the queue whilst it has already been scheduled for awakening. Either case however
        // demands (re)insertion into the queue.
        if let Some(node) = unsafe {
            Node::reuse_or_realloc(
                node,
                cx.waker().clone(),
                self.observer_data.static_area & SA_MASK_TYPE == SA_TYPE_WAIT_CHANGED,
            )
        } {
            let node_ptr: *mut Node = node;
            let prev_head = self
                .core
                .change_waiters_head
                .swap(node_ptr as usize, Ordering::AcqRel);

            // Safety: No data initialization depends on the ordering of this.
            node.next.store(prev_head, Ordering::Relaxed);

            let node: *mut Node = node;
            self.observer_data.static_area = (node as usize) | SA_TYPE_WAIT_CHANGED;
        }

        self.init = true;

        Poll::Pending
    }
}

impl<'a, T> Drop for MyWaitChangedFut<'a, T> {
    fn drop(&mut self) {
        if !self.init {
            return;
        }

        let node = (self.observer_data.static_area & SA_MASK_PTR) as _;

        if unsafe { Node::cancel(node) } {
            return;
        }

        unsafe {
            let next_node = Node::assume_wait_changed_next_node(node);
            if next_node != null_mut() {
                Node::assume_wait_changed_wake(next_node);
            }
        }
    }
}
