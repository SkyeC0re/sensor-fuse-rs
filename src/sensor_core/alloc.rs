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
    ptr::NonNull,
    sync::atomic::AtomicU8,
};

use async_lock::{
    futures::{Read, Write},
    RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use either::Either;
use futures::FutureExt;

use crate::{SymResult, Version};

use super::{SensorCore, SensorCoreAsync, CLOSED_BIT, VERSION_BUMP};

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

    // Safety:  `Node::cancel` relies on the fact that these are sequential.
    const STATE_CANCEL_BIT: u8 = 0b10;
    const STATE_DROP_BIT: u8 = 0b100;

    const STATE_COMPLETE_BIT: u8 = 0b1000;

    const NEXT_DATA_MASK: usize = 0b11;
    // Whether the node in the Read-Write is a write node.
    const NEXT_IS_WRITER_BIT: usize = 0b10;
    // Sentinal bit used for various purposes depending on what context the node is being used in.
    const NEXT_SENTINEL_BIT: usize = 0b1;
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

    /// Cancels or detaches the node if it has not been completed yet.
    ///
    /// Returns the waker if our cancellation or detaching was succesful. In this case the cancellation or detachment
    ///  was announced before the completion could occur, otherwise the completion has occured with the
    /// implications which follows from it.
    ///
    /// # Safety
    ///
    /// It is undefined behaviour to call this twice on the same node without resetting it.
    ///
    /// If `detach = true`, it is also undefined behaviour to access the node again if the waker was returned.
    #[inline(always)]
    unsafe fn cancel(node: *mut Self) -> Option<Waker> {
        let node = &mut *node;

        let mut state = node.state.load(Ordering::Acquire);
        debug_assert_eq!(state & Self::STATE_CANCEL_BIT, 0);

        if state & (Self::STATE_COMPLETE_BIT) != 0 {
            // We have already been completed. Nothing to do.
            return None;
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
            return None;
        }

        // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
        // of dropping the waker.
        let waker = node.waker.assume_init_read();
        node.state.store(
            state ^ (Self::STATE_LOCK_BIT | Self::STATE_CANCEL_BIT),
            Ordering::Release,
        );

        Some(waker)
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
        match self.try_prioritized_read() {
            Some(guard) => mem::forget(guard),
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
                    None => {
                        // Safety: We have two read guards, and can simply decrease the reader count here.
                        let _ = self.readers.fetch_sub(1, Ordering::Relaxed);
                        return;
                    }
                }
            }

            debug_assert_eq!(state, 0);

            // Uncancelled write detected. Stop waking here.
            if node_ptr & Node::NEXT_IS_WRITER_BIT != 0 {
                // Safety: We have two read guards, and can simply decrease the reader count here.
                let _ = self.readers.fetch_sub(1, Ordering::Relaxed);

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

            // Implicitly pass the extrenuous read guard to the awoken node.
            return;
        }
    }

    /// # Safety
    ///
    /// If `holds_write_guard` is true caller is implicity passing a write guard to this function.
    #[inline(always)]
    unsafe fn wake_next_in_queue(&self, holds_write_guard: bool) {
        let mut node_ptr = self.queue_head.load(Ordering::Acquire);

        if node_ptr <= Node::NEXT_SENTINEL_BIT {
            // Either the queue is empty or someone else has taken the responsibility to wake the next element.
            return;
        }

        // There is something in the queue. Either it is because we are holding a write guard, or it is likely a write
        // following some reads. The common path of this latter case combined with the guarantees we get by having
        // a write guard before setting the sentinel bit, makes this a good place to force a write guard if we do not
        // already have one. If we cannot aqcuire one now, then it becomes whoever is blocking us' responsibility to wake.
        if !holds_write_guard {
            match self.try_prioritized_write() {
                Some(guard) => mem::forget(guard),
                None => return,
            }
        }

        node_ptr = self.queue_head.load(Ordering::Acquire);

        if node_ptr == 0 {
            // Someone has intercepted us. Attempt a forceful write guard drop, blocking
            // new elements from entering the queue.
            match self.queue_head.compare_exchange(
                0,
                Node::NEXT_SENTINEL_BIT,
                Ordering::Relaxed,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.readers.store(0, Ordering::Relaxed);
                    self.queue_head.store(0, Ordering::Release);
                    return;
                }
                Err(new_node_ptr) => {
                    // We have a write guard, sentinel bit could not have been set.
                    debug_assert_ne!(new_node_ptr, Node::NEXT_SENTINEL_BIT);

                    node_ptr = new_node_ptr;
                }
            }
        }

        loop {
            let node = (node_ptr & Node::NEXT_PTR_MASK) as *mut Node;
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

                if let Some(next_node_ptr) = next_node_ptr {
                    node_ptr = next_node_ptr;
                    continue;
                }

                // List is cleared. We have a write guard. Attempt a forceful write guard drop.
                match self.queue_head.compare_exchange(
                    0,
                    Node::NEXT_SENTINEL_BIT,
                    Ordering::Relaxed,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        self.readers.store(0, Ordering::Relaxed);
                        self.queue_head.store(0, Ordering::Release);
                        return;
                    }
                    Err(new_node_ptr) => {
                        // We have a write guard, sentinel bit could not have been set.
                        debug_assert_ne!(new_node_ptr, Node::NEXT_SENTINEL_BIT);

                        node_ptr = new_node_ptr;
                        continue;
                    }
                }
            }

            debug_assert_eq!(state, 0);

            // Appropriately downgrade the lock for readers.
            if node_ptr & Node::NEXT_IS_WRITER_BIT == 0 {
                self.readers.store(1, Ordering::Relaxed);
            }

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

        // We were just the last reader. Try to reacquire ourselves, to maintain the invariants of `wake_next_in_queue`. If
        // we could not, then someone else is holding the lock and it is no longer our responsibility to wake the next element
        // in the queue.
        if self
            .core
            .readers
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
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

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum StaticRegionState {
    Uninitialized = 0,
    Free = 1,
    Write = 2,
    Read = 3,
    IdleRead = 4,
    WaitChanged = 5,
}

struct StaticRegion {
    node: *mut Node,
    state: StaticRegionState,
}

impl StaticRegion {
    #[inline(always)]
    const fn new() -> Self {
        Self {
            node: null_mut(),
            state: StaticRegionState::Uninitialized,
        }
    }

    #[inline(always)]
    fn ensure_init(&mut self) {
        if self.state == StaticRegionState::Uninitialized {
            self.node = Box::into_raw(Box::new(Node {
                waker: MaybeUninit::uninit(),
                next: AtomicUsize::new(0),
                state: AtomicU8::new(Node::STATE_COMPLETE_BIT),
            }));
            self.state = StaticRegionState::Free;
        }
    }
}

struct MyObserver<T, R: Deref<Target = MySensorCore<T>>> {
    core: R,
    static_area: StaticRegion,
    version: Version,
}

struct ObserverData {
    static_area: usize,
    version: Version,
}

impl<T, R: Deref<Target = MySensorCore<T>>> MyObserver<T, R> {
    pub fn wait_changed(&mut self) -> MyWaitChangedFut<T> {
        self.static_area.ensure_init();
        MyWaitChangedFut {
            core: &self.core,
            static_area: &mut self.static_area,
            version: self.version,
            init: false,
        }
    }

    pub fn read(&mut self) -> MyReadFut<T> {
        self.static_area.ensure_init();
        MyReadFut {
            static_area: &mut self.static_area,
            core: &self.core,
            init: false,
        }
    }

    pub fn wait_for<F: FnMut(&T) -> bool>(&mut self, condition: F) -> MyWaitForFut<T, F> {
        self.static_area.ensure_init();
        MyWaitForFut {
            version: self.version,
            condition,
            read: self.read(),
        }
    }
}

struct MyReadFut<'a, T> {
    static_area: &'a mut StaticRegion,
    core: &'a MySensorCore<T>,
    init: bool,
}

impl<'a, T> Future for MyReadFut<'a, T> {
    type Output = ReadGuard<'a, T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = self.static_area.node;
        if self.init {
            if unsafe { (*node).state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0 } {
                return Poll::Pending;
            }

            unsafe {
                if let Some(next_ptr) = self.core.try_reset_queue(node as usize) {
                    self.core.wake_next_reader(next_ptr);
                }
            }

            self.init = false;

            // Whoever woke us has generated a read guard for us.
            return Poll::Ready(ReadGuard { core: self.core });
        }

        if let Some(guard) = self.core.try_read() {
            return Poll::Ready(guard);
        }

        let node_type = self.static_area.state;
        let new_static_node = if let Some(node) = unsafe {
            Node::reuse_or_realloc(
                node,
                cx.waker().clone(),
                node_type == StaticRegionState::Read || node_type == StaticRegionState::Write,
            )
        } {
            let node_as_ptr: *mut Node = node;
            let old_tail = self
                .core
                .queue_tail
                // `AcqRel` needed here to ensure `reuse_or_realloc` invariants for both this node and the old tail.
                .swap(node_as_ptr as usize, Ordering::AcqRel);

            if old_tail == 0 {
                // Ensure that the head is also properly zeroed. This will block on both a head still being cleared out and a sentinel bit being set on
                // a zeroed head.
                while self
                    .core
                    .queue_head
                    .compare_exchange_weak(
                        0,
                        node_as_ptr as usize,
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    spin_loop();
                }

                // We are the new queue head, see if we can awake immediately, in case the lock has become inactive whilst we were inserted.
                if let Some(guard) = self.core.try_prioritized_read() {
                    unsafe {
                        // We are now responsible for waking.
                        if let Some(next_node) = self.core.try_reset_queue(node_as_ptr as usize) {
                            self.core.wake_next_reader(next_node);
                        }

                        // Safety: We again have exlusive access to our node.
                        node.waker.assume_init_drop();
                        *node.state.get_mut() = Node::STATE_COMPLETE_BIT;

                        *self.static_area = StaticRegion {
                            node,
                            state: StaticRegionState::Read,
                        };

                        return Poll::Ready(guard);
                    }
                }
            } else {
                unsafe {
                    (*((old_tail & Node::NEXT_PTR_MASK) as *mut Node))
                        .next
                        .store(node_as_ptr as usize, Ordering::Release)
                };
            }

            node_as_ptr
        } else {
            node
        };

        *self.static_area = StaticRegion {
            node: new_static_node,
            state: StaticRegionState::Read,
        };
        self.init = true;

        Poll::Pending
    }
}

impl<'a, T> Drop for MyReadFut<'a, T> {
    fn drop(&mut self) {
        if !self.init {
            return;
        }

        let node = self.static_area.node;

        if unsafe { Node::cancel(node).is_some() } {
            return;
        }

        // It is our responsibility to wake the next element in queue if any, and to drop the read guard
        // that was passed to us.
        unsafe {
            if let Some(next_ptr) = self.core.try_reset_queue(node as usize) {
                self.core.wake_next_reader(next_ptr);
            }
        }
        drop(ReadGuard { core: self.core });
    }
}

struct MyWaitForFut<'a, T, F: FnMut(&T) -> bool> {
    condition: F,
    read: MyReadFut<'a, T>,
    version: Version,
}

impl<'a, T, F: FnMut(&T) -> bool> Future for MyWaitForFut<'a, T, F> {
    type Output = (ReadGuard<'a, T>, Version);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: Only `F` is potentially unpin which does not move.
        let s = unsafe { self.get_unchecked_mut() };

        let guard = match s.read.poll_unpin(cx) {
            Poll::Ready(guard) => guard,
            Poll::Pending => return Poll::Pending,
        };

        let new_version = s.read.core.version();
        if new_version.closed_bit_set() || (new_version != s.version && (s.condition)(&guard)) {
            return Poll::Ready((guard, new_version));
        }
        s.version = new_version;

        let core = s.read.core;
        let node = s.read.static_area.node;

        let old_tail = core.idle_reads_tail.swap(node, Ordering::AcqRel);
        if old_tail.is_null() {
            // Ensure that the head is also properly zeroed.
            while core
                .idle_reads_head
                .compare_exchange_weak(null_mut(), node, Ordering::Release, Ordering::Relaxed)
                .is_err()
            {
                spin_loop();
            }
        } else {
            unsafe {
                (*old_tail).next.store(node as usize, Ordering::Release);
            }
        }

        // We do not have to check for a version change here. It may have occurred, but not due to actual
        // value changes. As such we are allowed to pretend that the condition is still false.

        *s.read.static_area = StaticRegion {
            node,
            state: StaticRegionState::IdleRead,
        };
        s.read.init = true;

        Poll::Pending
    }
}

struct MyWriteFut<'a, T> {
    static_area: &'a mut StaticRegion,
    core: &'a MySensorCore<T>,
    init: bool,
}

impl<'a, T> Future for MyWriteFut<'a, T> {
    type Output = WriteGuard<'a, T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = self.static_area.node;
        if self.init {
            if unsafe { (*node).state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0 } {
                return Poll::Pending;
            }

            self.init = false;

            // Whoever woke us has generated a write guard for us.
            return Poll::Ready(WriteGuard { core: self.core });
        }

        if let Some(guard) = self.core.try_write() {
            return Poll::Ready(guard);
        }

        let node_type = self.static_area.state;
        let new_static_node = if let Some(node) = unsafe {
            Node::reuse_or_realloc(
                node,
                cx.waker().clone(),
                node_type == StaticRegionState::Read || node_type == StaticRegionState::Write,
            )
        } {
            let node_as_ptr: *mut Node = node;
            let typed_static_area = node_as_ptr as usize | Node::NEXT_IS_WRITER_BIT;
            let old_tail = self
                .core
                .queue_tail
                // `AcqRel` needed here to ensure `reuse_or_realloc` invariants for both this node and the old tail.
                .swap(typed_static_area, Ordering::AcqRel);

            if old_tail == 0 {
                // Ensure that the head is also properly zeroed. This will block on both a head still being cleared out and a sentinel bit being set on
                // a zeroed head.
                while self
                    .core
                    .queue_head
                    .compare_exchange_weak(
                        0,
                        typed_static_area,
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    spin_loop();
                }

                // We are the new queue head, see if we can awake immediately, in case the lock has become inactive whilst we were inserted.
                if let Some(guard) = self.core.try_prioritized_write() {
                    unsafe {
                        // Safety: We again have exlusive access to our node.
                        node.waker.assume_init_drop();
                        *node.state.get_mut() = Node::STATE_COMPLETE_BIT;

                        *self.static_area = StaticRegion {
                            node,
                            state: StaticRegionState::Write,
                        };
                    }

                    return Poll::Ready(guard);
                }
            } else {
                unsafe {
                    (*((old_tail & Node::NEXT_PTR_MASK) as *mut Node))
                        .next
                        .store(node_as_ptr as usize, Ordering::Release)
                };
            }

            node_as_ptr
        } else {
            node
        };

        *self.static_area = StaticRegion {
            node: new_static_node,
            state: StaticRegionState::Write,
        };
        self.init = true;

        Poll::Pending
    }
}

impl<'a, T> Drop for MyWriteFut<'a, T> {
    fn drop(&mut self) {
        if !self.init {
            return;
        }

        if unsafe { Node::cancel(self.static_area.node).is_some() } {
            return;
        }

        // It is our responsibility to drop the write guard that was passed to us.
        drop(WriteGuard { core: self.core });
    }
}

struct MyWaitChangedFut<'a, T> {
    static_area: &'a mut StaticRegion,
    core: &'a MySensorCore<T>,
    version: Version,
    init: bool,
}

impl<'a, T> Future for MyWaitChangedFut<'a, T> {
    type Output = SymResult<Version>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = self.static_area.node;
        if self.init {
            if unsafe { (*node).state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0 } {
                return Poll::Pending;
            }

            let curr_version = self.core.version();
            if self.version != curr_version {
                // Do not rely on `Drop` trait to wake the next element. It may not be instantaneous.
                unsafe {
                    if Node::cancel(node).is_none() {
                        let next_node = Node::assume_wait_changed_next_node(node);
                        if next_node != null_mut() {
                            Node::assume_wait_changed_wake(node);
                        }
                    }
                }

                self.init = false;
                return Poll::Ready(curr_version.as_result());
            }
        } else {
            let curr_version = self.core.version();
            if self.version != curr_version {
                return Poll::Ready(curr_version.as_result());
            }
        }

        // The version was unchanged. If `init == true` then we were not spuriosly awoken, which can only happen if
        // we reused a canceled node in the queue whilst it has already been scheduled for awakening. Either case however
        // demands (re)insertion into the queue.
        if let Some(node) = unsafe {
            Node::reuse_or_realloc(
                node,
                cx.waker().clone(),
                self.static_area.state == StaticRegionState::WaitChanged,
            )
        } {
            let node_ptr: *mut Node = node;
            let prev_head = self
                .core
                .change_waiters_head
                .swap(node_ptr as usize, Ordering::AcqRel);

            // Safety: No data initialization depends on the ordering of this.
            node.next.store(prev_head, Ordering::Relaxed);
            *self.static_area = StaticRegion {
                node,
                state: StaticRegionState::WaitChanged,
            };
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

        let node = self.static_area.node;

        if unsafe { Node::cancel(node).is_some() } {
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
