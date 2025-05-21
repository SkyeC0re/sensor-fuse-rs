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
use futures::FutureExt;

use crate::{SensorWrite, SensorWriteAsync, SymResult, Version};

use super::{SensorCore, SensorCoreAsync, CLOSED_BIT, VERSION_BUMP};

macro_rules! miri_log {
    ($($arg:tt)*) => {
        #[cfg(feature = "dev-miri-logs")]
        println!($($arg)*)
    };
}

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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[repr(u8)]
enum NodeType {
    Free = 0b0,
    WaitChanged = 0b1,
    Read = 0b10,
    PassiveRead = 0b100,
    Write = 0b1000,
}

#[derive(Debug)]
#[repr(align(4))]
struct Node {
    waker: MaybeUninit<Waker>,
    next: AtomicPtr<Node>,
    // ... | COMPLETE_BIT | CANCEL_BIT | DROP_BIT | LOCK_BIT
    state: AtomicU8,
    tp: NodeType,
}

impl Node {
    const STATE_LOCK_BIT: u8 = 0b1;
    const STATE_CANCEL_BIT: u8 = 0b10;
    const STATE_DROP_BIT: u8 = 0b100;
    const STATE_COMPLETE_BIT: u8 = 0b1000;

    /// Sentinel pointer. Do **not** access.
    #[inline(always)]
    const unsafe fn sentinel() -> *mut Node {
        1 as _
    }

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
        new_type: NodeType,
        allow_reset_on: u8,
    ) -> Option<&'static mut Self> {
        // let old_node = &mut *node;

        // Write free initial check for completion.
        let mut state = (*node).state.load(Ordering::Acquire);
        if state & Self::STATE_COMPLETE_BIT != 0 {
            // Free to re-use mutably. We are the sole owners.
            Node::assume_exclusive_reset(node, waker, new_type);
            return Some(&mut *node);
        }

        state = Self::lock(node);

        if state & Self::STATE_COMPLETE_BIT != 0 {
            // Free to re-use mutably. We are the sole owners.
            Node::assume_exclusive_reset(node, waker, new_type);
            return Some(&mut *node);
        }

        debug_assert_eq!(state, Self::STATE_CANCEL_BIT);

        if allow_reset_on & (*node).tp as u8 != 0 {
            // Piggyback off of the fact that the node is still in an appropriate queue.
            let _ = (*node).waker.write(waker);
            (*node).tp = new_type;
            (*node).state.store(0, Ordering::Release);

            return None;
        }

        // No attempts at recycling the allocation succeeded. Dump it and acquire a new one.
        (*node).state.store(Self::STATE_DROP_BIT, Ordering::Release);

        let node = Box::new(Node {
            waker: MaybeUninit::new(waker),
            next: AtomicPtr::new(Node::sentinel()),
            state: AtomicU8::new(0),
            tp: new_type,
        });

        return Some(unsafe { &mut *Box::into_raw(node) });
    }

    /// Reset the node's values for a new future that is to be added to a queue.
    ///
    /// # Safety
    ///
    /// Behaviour is undefined if node is not in a completed (i.e. exclusive) state.
    #[inline(always)]
    unsafe fn assume_exclusive_reset(node: *mut Self, waker: Waker, new_type: NodeType) {
        let node = &mut *node;
        let _ = node.waker.write(waker);
        *node.next.get_mut() = Node::sentinel();
        *node.state.get_mut() = 0;
        node.tp = new_type;
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
        let mut state = (*node).state.load(Ordering::Acquire);
        debug_assert_eq!(state & Self::STATE_CANCEL_BIT, 0);

        if state & (Self::STATE_COMPLETE_BIT) != 0 {
            // We have already been completed. Nothing to do.
            return None;
        }

        // A perceived lock state does not guarantee that we are about to be completed (as is the case for a write node).
        // Wait until we have a lock to guarantee our state.
        state = Node::lock(node);

        if state & Self::STATE_COMPLETE_BIT != 0 {
            // We have already been completed. Nothing to do.
            return None;
        }

        // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
        // of dropping the waker.
        let waker = (*node).waker.assume_init_read();
        (*node)
            .state
            .store(state | Self::STATE_CANCEL_BIT, Ordering::Release);

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

            if next_ptr != Node::sentinel() {
                return next_ptr;
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
        let state = Node::lock(node);

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
            .store(Self::STATE_COMPLETE_BIT, Ordering::Release);
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

        loop {
            while state & Self::STATE_LOCK_BIT != 0 {
                spin_loop();
                state = (*node).state.load(Ordering::Relaxed);
            }

            match (*node).state.compare_exchange(
                state,
                state | Self::STATE_LOCK_BIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => state = e,
            }
        }

        state
    }
}

pub struct MySensorCore<T> {
    queue_head: AtomicPtr<Node>,
    queue_tail: AtomicPtr<Node>,
    idle_reads_head: AtomicPtr<Node>,
    idle_reads_tail: AtomicPtr<Node>,
    change_waiters_head: AtomicPtr<Node>,
    num_writers: AtomicUsize,
    writes_queued: AtomicUsize,
    lock_state: AtomicUsize,
    version: AtomicUsize,
    data: UnsafeCell<T>,
}

unsafe impl<T> Sync for MySensorCore<T> where T: Send + Sync {}

impl<T> MySensorCore<T> {
    #[inline(always)]
    fn version(&self) -> Version {
        Version(self.version.load(Ordering::Relaxed))
    }

    #[inline(always)]
    fn wake_waiters(&self) {
        // Swap the tail sentinal value in, taking effective ownership of the current queue and allowing
        // the next queue to start populating.
        // `Ordering::AcqRel` is not needed here, the sentinal indicator is sufficient.
        let node = self.change_waiters_head.swap(null_mut(), Ordering::Acquire);

        if node == null_mut() {
            return;
        }

        unsafe {
            Node::assume_wait_changed_wake(node as *mut _);
        }
    }

    #[inline(always)]
    fn activate_idle_reads(&self) {
        let idle_head = self.idle_reads_head.swap(null_mut(), Ordering::Acquire);

        if idle_head == null_mut() {
            return;
        }

        // Safety, when a new node is inserted, the head is set last with an acquire ordering. This cannot be zero
        // if the head was non-zero.
        let idle_tail = self.idle_reads_tail.swap(null_mut(), Ordering::Relaxed);

        let old_tail = self.queue_tail.swap(idle_tail, Ordering::AcqRel);

        assert_ne!(idle_head, unsafe { Node::sentinel() });

        if old_tail.is_null() {
            // Ensure that the head is also properly zeroed. This will block on both a head still being cleared out and a sentinel bit being set on
            // a zeroed head.
            while self
                .queue_head
                .compare_exchange_weak(null_mut(), idle_head, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                spin_loop();
            }

            let guard = match self.try_prioritized_read() {
                Some(guard) => guard,
                None => {
                    return;
                }
            };

            if self
                .queue_head
                .compare_exchange(
                    idle_head,
                    unsafe { Node::sentinel() },
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                return;
            }

            unsafe { self.wake_next_reader(idle_head, guard) };
        } else {
            unsafe { (*old_tail).next.store(idle_head, Ordering::Release) };
        }
    }

    #[inline]
    fn bump_version(&self) {
        let _ = self.version.fetch_add(VERSION_BUMP, Ordering::Relaxed);

        self.wake_waiters();
        self.activate_idle_reads();
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
        let mut lock_state = self.lock_state.load(Ordering::Relaxed);
        while lock_state < usize::MAX - 1 {
            match self.lock_state.compare_exchange_weak(
                lock_state,
                lock_state + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(ReadGuard { core: self }),
                Err(e) => lock_state = e,
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
        match self
            .lock_state
            .compare_exchange(0, usize::MAX, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => {}
            Err(_e) => {
                miri_log!("write guard acq fail, lock state: {_e}");
                return None;
            }
        }

        Some(WriteGuard { core: self })
    }

    /// # Safety
    ///
    /// - `node` must point to an actual node in the Read-Write queue.
    /// - `node` must be properly initialized from the perspective of the thread calling this function.
    #[inline(always)]
    unsafe fn wake_next_reader(&self, mut node: *mut Node, guard: ReadGuard<T>) {
        loop {
            let state = Node::lock(node);

            if state & (Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT) != 0 {
                debug_assert_eq!(state & !(Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT), 0);

                let next_node_ptr = self.try_reset_queue_tail(node);

                if state == Node::STATE_DROP_BIT {
                    drop(Box::from_raw(node));
                } else {
                    (*node)
                        .state
                        .store(state | Node::STATE_COMPLETE_BIT, Ordering::Release);
                }

                if next_node_ptr.is_null() {
                    // Nothing to do, list is cleared.
                    self.queue_head.store(null_mut(), Ordering::Release);

                    return;
                } else {
                    node = next_node_ptr;
                    continue;
                }
            }

            debug_assert_eq!(state, 0);

            // Uncancelled write detected. Stop waking here.
            if (*node).tp == NodeType::Write {
                self.queue_head.store(node, Ordering::Release);

                (*node).state.store(0, Ordering::Release);
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
            mem::forget(guard);
            return;
        }
    }

    /// # Safety
    ///
    /// The caller must have a write guard that they are implicitly passing to this function.
    #[inline(always)]
    unsafe fn wake_next_in_queue(&self) {
        let head_node = if let Err(e) = self.queue_head.compare_exchange(
            null_mut(),
            Node::sentinel(),
            Ordering::Relaxed,
            Ordering::Acquire,
        ) {
            e
        } else {
            // Queue is now empty. Open the lock, and **then** allow new elements into the queue.
            self.lock_state.store(0, Ordering::Release);
            self.queue_head.store(null_mut(), Ordering::Release);
            return;
        };

        self.wake_next_with_write_guard(head_node);
    }

    #[inline(always)]
    unsafe fn wake_next_with_write_guard(&self, mut node: *mut Node) {
        loop {
            let state = Node::lock(node);

            if state & (Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT) != 0 {
                debug_assert_eq!(state & !(Node::STATE_DROP_BIT | Node::STATE_CANCEL_BIT), 0);

                // Only now is it our responsibility to find the next element.
                let next_node = self.try_reset_queue_tail(node);

                if state == Node::STATE_DROP_BIT {
                    drop(Box::from_raw(node));
                } else {
                    (*node)
                        .state
                        .store(state | Node::STATE_COMPLETE_BIT, Ordering::Release);
                }

                if !next_node.is_null() {
                    node = next_node;
                    continue;
                }

                self.lock_state.store(0, Ordering::Release);
                self.queue_head.store(null_mut(), Ordering::Release);
                return;
            }

            debug_assert_eq!(state, 0);

            if (*node).tp != NodeType::Write {
                // Appropriately downgrade the lock for reads.
                self.lock_state.store(1, Ordering::Release);
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

    /// Try to reset the Read-Write queue tail given a non-zero typed node that is currently in the queue.
    ///
    /// # Safety
    ///
    ///  - Must be called only by the thread who has a read or write guard and who's responsibility it is to advance the queue.
    ///  - `possible_tail_node` should point to a valid node inside the the Read-Write queue.
    ///  - `possible_tail_node` must be properly initialized from the perspective of the thread calling this function.
    #[inline(always)]
    unsafe fn try_reset_queue_tail(&self, possible_tail_node: *mut Node) -> *mut Node {
        // Ensure that the next node's data is populated before any access to it.
        let mut next = (*possible_tail_node).next.load(Ordering::Acquire);
        if next != Node::sentinel() {
            return next;
        }

        if self
            .queue_tail
            .compare_exchange(
                possible_tail_node,
                null_mut(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return null_mut();
        };

        // A new node has attached to the tail, we cannot reset the queue.

        loop {
            // Ensure that the next node's data is populated before any access to it.
            next = (*possible_tail_node).next.load(Ordering::Acquire);
            if next != Node::sentinel() {
                return next;
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
        let lock_state = self.core.lock_state.fetch_sub(1, Ordering::Release);

        debug_assert!(lock_state != 0);

        miri_log!("read guard drop, lock state: {lock_state}");

        if lock_state != 1 {
            return;
        }

        // We need release ordering on success here to ensure that when a node announces itself as the head,
        // that it will observe that the lock has been freed by us.
        if self
            .core
            .queue_head
            .compare_exchange(null_mut(), null_mut(), Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }

        // It is fine to attempt aquisition of a write guard at this stage, as it is highly likely that the
        // queued element is a write node. If we cannot aqcuire the lock, then it is whoever has prevented us'
        // responsibility to wake.
        if let Some(guard) = self.core.try_prioritized_write() {
            mem::forget(guard);
            unsafe { self.core.wake_next_in_queue() };
        }

        miri_log!("WNIQNG end");
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
        miri_log!("write guard drop");
        unsafe { self.core.wake_next_in_queue() };
    }
}

struct StaticRegion(*mut Node);

// Safety: we guarantee that `node` always represents an allocated `Node` value
// generated by `Box::into_raw` (or null).
unsafe impl Send for StaticRegion {}
unsafe impl Sync for StaticRegion {}

impl StaticRegion {
    #[inline(always)]
    const fn new() -> Self {
        Self(null_mut())
    }

    #[inline(always)]
    fn ensure_init(&mut self) {
        if self.0 == null_mut() {
            self.0 = Box::into_raw(Box::new(Node {
                waker: MaybeUninit::uninit(),
                next: AtomicPtr::new(unsafe { Node::sentinel() }),
                state: AtomicU8::new(Node::STATE_COMPLETE_BIT),
                tp: NodeType::Free,
            }));
        }
    }

    #[inline(always)]
    const fn init(&self) -> bool {
        !self.0.is_null()
    }
}

pub struct MyObserver<T, R: Deref<Target = MySensorCore<T>>> {
    core: R,
    static_area: StaticRegion,
    version: Version,
}

impl<T, R: Deref<Target = MySensorCore<T>>> MyObserver<T, R> {
    #[inline]
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

    #[inline]
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
        let node = self.static_area.0;
        if self.init {
            if unsafe { (*node).state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0 } {
                miri_log!("read spurious: {:?}", self.static_area.0);
                return Poll::Pending;
            }

            unsafe {
                let next_ptr = self.core.try_reset_queue_tail(node);
                if next_ptr.is_null() {
                    self.core.queue_head.store(null_mut(), Ordering::Release);
                } else if let Some(guard) = self.core.try_prioritized_read() {
                    self.core.wake_next_reader(next_ptr, guard);
                } else {
                    self.core.queue_head.store(next_ptr, Ordering::Release);
                }
            }

            self.init = false;
            miri_log!("read awoken: {:?}", self.static_area.0);
            // Whoever woke us has generated a read guard for us.
            return Poll::Ready(ReadGuard { core: self.core });
        }

        if let Some(guard) = self.core.try_read() {
            return Poll::Ready(guard);
        }

        if let Some(node) = unsafe {
            Node::reuse_or_realloc(
                node,
                cx.waker().clone(),
                NodeType::Read,
                NodeType::Read as u8 | NodeType::Write as u8,
            )
        } {
            assert_eq!(*node.state.get_mut(), 0);
            self.static_area.0 = node;

            let old_tail = self
                .core
                .queue_tail
                // `AcqRel` needed here to ensure `reuse_or_realloc` invariants for both this node and the old tail.
                .swap(node, Ordering::AcqRel);

            if old_tail.is_null() {
                // Ensure that the head is also properly zeroed. This will block on both a head still being cleared out and a sentinel bit being set on
                // a zeroed head.
                //
                // We need `Ordering::AcqRel` on success here to ensure that we announce our local modifications of `Node::reuse_or_realloc`
                // and to ensure that if a write guard was just dropped, that we see that fact when we attempt to acquire a read guard
                // right after announcing ourselves as the queue head. Otherwise, it is possible to perceive the lock as still being in the
                // write state even after the queue head is zeroed.
                while self
                    .core
                    .queue_head
                    .compare_exchange_weak(null_mut(), node, Ordering::AcqRel, Ordering::Relaxed)
                    .is_err()
                {
                    spin_loop();
                }

                // We are the new queue head, see if we can awake immediately, in case the lock has become inactive whilst we were inserted.
                if let Some(guard) = self.core.try_prioritized_read() {
                    unsafe {
                        // We have taken on the responsibility to wake.
                        // We know that someone waking the queue must have gotten a write guard first.
                        // As such the only options for reaching this state is that no one is waking the queue,
                        // or have just downgraded the writeguard for us an we are currently locked. In the former,
                        // we should be able to cancel and be guaranteed that no one will attempt to wake us, giving
                        // us exclusive access to our node again, or someone has just downgraded the guard for us, and
                        // our cancellation attempt will fail.
                        let extra_guard = match Node::cancel(node) {
                            Some(_) => {
                                *(*node).state.get_mut() = Node::STATE_COMPLETE_BIT;
                                None
                            }
                            None => Some(ReadGuard { core: self.core }),
                        };

                        let next_node = self.core.try_reset_queue_tail(node);

                        if next_node.is_null() {
                            self.core.queue_head.store(null_mut(), Ordering::Release);
                        } else if let Some(extra_guard) =
                            extra_guard.or_else(|| self.core.try_prioritized_read())
                        {
                            self.core.wake_next_reader(next_node, extra_guard);
                        } else {
                            self.core.queue_head.store(next_node, Ordering::Release);
                        }

                        miri_log!("read self wake: {:?}", self.static_area.0);
                        return Poll::Ready(guard);
                    }
                }
            } else {
                unsafe { (*old_tail).next.store(node, Ordering::Release) };
            }
        }

        miri_log!("read pending: {:?}", self.static_area.0);
        self.init = true;

        Poll::Pending
    }
}

impl<'a, T> Drop for MyReadFut<'a, T> {
    fn drop(&mut self) {
        if !self.init {
            return;
        }

        let node = self.static_area.0;

        if unsafe { Node::cancel(node).is_some() } {
            return;
        }

        let guard = ReadGuard { core: self.core };

        // It is our responsibility to wake the next element in queue if any, and to drop the read guard
        // that was passed to us.
        unsafe {
            let next_node = self.core.try_reset_queue_tail(node);
            if next_node.is_null() {
                self.core.queue_head.store(null_mut(), Ordering::Release);
            } else {
                self.core.wake_next_reader(next_node, guard);
            }
        }
    }
}

pub struct MyWaitForFut<'a, T, F: FnMut(&T) -> bool> {
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
        let node: *mut Node = unsafe {
            Node::reuse_or_realloc(
                s.read.static_area.0,
                cx.waker().clone(),
                NodeType::PassiveRead,
                NodeType::PassiveRead as u8,
            )
            .unwrap()
        };
        s.read.static_area.0 = node;

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
                (*old_tail).next.store(node, Ordering::Release);
            }
        }

        // We do not have to check for a version change here. It may have occurred, but not due to actual
        // value changes. As such we are allowed to pretend that the condition is still false.

        s.read.init = true;

        Poll::Pending
    }
}

pub struct MyWriter<T, R: Deref<Target = MySensorCore<T>>> {
    core: R,
    static_area: StaticRegion,
}

impl<T, R: Deref<Target = MySensorCore<T>> + Clone> Clone for MyWriter<T, R> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            static_area: StaticRegion::new(),
        }
    }
}

impl<T, R: Deref<Target = MySensorCore<T>>> MyWriter<T, R> {
    pub fn write(&mut self) -> MyWriteFut<T> {
        self.static_area.ensure_init();
        MyWriteFut {
            core: &self.core,
            static_area: &mut self.static_area,
            init: false,
        }
    }
}

impl<T> MyWriter<T, Arc<MySensorCore<T>>> {
    pub fn new(init: T) -> Self {
        Self {
            core: Arc::new(MySensorCore {
                queue_head: AtomicPtr::new(null_mut()),
                queue_tail: AtomicPtr::new(null_mut()),
                idle_reads_head: AtomicPtr::new(null_mut()),
                idle_reads_tail: AtomicPtr::new(null_mut()),
                change_waiters_head: AtomicPtr::new(null_mut()),
                num_writers: AtomicUsize::new(1),
                writes_queued: AtomicUsize::new(0),
                lock_state: AtomicUsize::new(0),
                version: AtomicUsize::new(0),
                data: UnsafeCell::new(init),
            }),
            static_area: StaticRegion::new(),
        }
    }
}

impl<T, R: Deref<Target = MySensorCore<T>> + Clone> MyWriter<T, R> {
    pub fn subscribe(&self) -> MyObserver<T, R> {
        let mut version = self.core.version();
        version.decrement();
        MyObserver {
            core: self.core.clone(),
            static_area: StaticRegion::new(),
            version,
        }
    }
}

impl<T, R: Deref<Target = MySensorCore<T>>> Drop for MyWriter<T, R> {
    fn drop(&mut self) {
        if self.static_area.init() {
            unsafe {
                if Node::cancel(self.static_area.0).is_none() {
                    drop(Box::from_raw(self.static_area.0));
                }
            }
        }

        if self.core.num_writers.fetch_sub(1, Ordering::Relaxed) != 1 {
            return;
        }

        let _ = self.core.version.fetch_or(CLOSED_BIT, Ordering::Relaxed);
        self.core.wake_waiters();
        self.core.activate_idle_reads();
    }
}

pub struct MyWriteFut<'a, T> {
    static_area: &'a mut StaticRegion,
    core: &'a MySensorCore<T>,
    init: bool,
}

impl<'a, T> Future for MyWriteFut<'a, T> {
    type Output = WriteGuard<'a, T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = self.static_area.0;
        if self.init {
            if unsafe { (*node).state.load(Ordering::Acquire) & Node::STATE_COMPLETE_BIT == 0 } {
                return Poll::Pending;
            }

            // Update the queue head to point to the next element if any.
            let next_node = unsafe { self.core.try_reset_queue_tail(node) };

            // Either there is a next node, or there is not, but both require an update of the queue head.
            self.core.queue_head.store(next_node, Ordering::Release);

            let _ = self.core.writes_queued.fetch_sub(1, Ordering::Relaxed);

            // Whoever woke us has generated a write guard for us.
            self.init = false;
            return Poll::Ready(WriteGuard { core: self.core });
        }

        if let Some(guard) = self.core.try_write() {
            return Poll::Ready(guard);
        }

        if let Some(node) = unsafe {
            Node::reuse_or_realloc(
                node,
                cx.waker().clone(),
                NodeType::Write,
                NodeType::Write as u8 | NodeType::Read as u8,
            )
        } {
            self.static_area.0 = node;

            let old_tail = self
                .core
                .queue_tail
                // `AcqRel` needed here to ensure `reuse_or_realloc` invariants for both this node and the old tail.
                .swap(node, Ordering::AcqRel);

            if old_tail.is_null() {
                // Ensure that the head is also properly zeroed, then announce ourselves as the new head.
                // This will block on both a head still being cleared out and a sentinel bit being set on
                // a zeroed head.
                while self
                    .core
                    .queue_head
                    .compare_exchange_weak(null_mut(), node, Ordering::Release, Ordering::Relaxed)
                    .is_err()
                {
                    spin_loop();
                }

                // We are the new queue head, see if we can awake immediately, in case the lock has become inactive whilst we were inserted.
                if let Some(guard) = self.core.try_prioritized_write() {
                    unsafe {
                        let next_node = self.core.try_reset_queue_tail(node);

                        assert_ne!(next_node, Node::sentinel());
                        self.core.queue_head.store(next_node, Ordering::Release);

                        // Safety: We again have exlusive access to our node.
                        node.waker.assume_init_drop();

                        assert_ne!(*node.state.get_mut(), Node::STATE_COMPLETE_BIT);
                        *node.state.get_mut() = Node::STATE_COMPLETE_BIT;
                    }

                    return Poll::Ready(guard);
                }
                miri_log!("write missed lock: {:?}", self.static_area.0);
            } else {
                unsafe { (*old_tail).next.store(node, Ordering::Release) };
            }
        }

        let _ = self.core.writes_queued.fetch_add(1, Ordering::Relaxed);
        self.init = true;

        miri_log!("write pending: {:?}", self.static_area.0);
        Poll::Pending
    }
}

impl<'a, T> Drop for MyWriteFut<'a, T> {
    fn drop(&mut self) {
        if !self.init {
            return;
        }

        let _ = self.core.writes_queued.fetch_sub(1, Ordering::Relaxed);

        if unsafe { Node::cancel(self.static_area.0).is_some() } {
            return;
        }

        let next_node = unsafe { self.core.try_reset_queue_tail(self.static_area.0) };

        // It is our responsibility to drop or pass the write guard that was passed to us.
        if next_node.is_null() {
            // We need to first downgrade to a read guard, then unset the sentinel then drop the read guard. If we do not do
            // it this way free the lock immediately, there is a chance that a reader could have come in as soon as we unlocked,
            // completed and gave up on waking when it saw the sentinel, leading to no awaking even with elements in the queue.
            self.core.lock_state.store(1, Ordering::Release);
            self.core.queue_head.store(null_mut(), Ordering::Release);
            drop(ReadGuard { core: self.core });
        } else {
            unsafe { self.core.wake_next_with_write_guard(next_node) };
        }
    }
}

pub struct MyWaitChangedFut<'a, T> {
    static_area: &'a mut StaticRegion,
    core: &'a MySensorCore<T>,
    version: Version,
    init: bool,
}

impl<'a, T> Future for MyWaitChangedFut<'a, T> {
    type Output = SymResult<Version>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = self.static_area.0;
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
                            Node::assume_wait_changed_wake(next_node);
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
                NodeType::WaitChanged,
                NodeType::WaitChanged as u8,
            )
        } {
            self.static_area.0 = node;

            let prev_head = self.core.change_waiters_head.swap(node, Ordering::AcqRel);

            // Safety: No data initialization depends on the ordering of this.
            node.next.store(prev_head, Ordering::Relaxed);
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

        let node = self.static_area.0;

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

impl<T, R: Deref<Target = MySensorCore<T>>> SensorWrite<T> for MyWriter<T, R> {
    type WriteGuard<'a>
        = WriteGuard<'a, T>
    where
        Self: 'a;

    #[inline(always)]
    fn notify_all(&self) {
        self.core.bump_version();
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.core.try_write()
    }
}

impl<T, R: Deref<Target = MySensorCore<T>>> SensorWriteAsync<T> for MyWriter<T, R> {
    #[allow(refining_impl_trait)]
    #[inline(always)]
    fn write(&mut self) -> MyWriteFut<'_, T> {
        self.write()
    }

    #[allow(refining_impl_trait)]
    #[inline(always)]
    async fn modify<F: FnOnce(&mut T) -> bool>(&mut self, f: F) {
        let mut guard = self.write().await;
        if f(&mut guard) {
            guard.core.bump_version();
        }
    }

    #[allow(refining_impl_trait)]
    #[inline(always)]
    async fn update(&mut self, value: T) {
        self.modify(|v| {
            *v = value;
            true
        })
        .await
    }
}

#[cfg(test)]
mod test {
    use std::{hint::spin_loop, thread, time::Duration};

    use futures::executor::block_on;

    use super::MyWriter;

    #[test]
    fn test_this() {
        let mut writer = MyWriter::new(5);

        let mut threads = Vec::new();
        for i in 0..2 {
            let mut reader = writer.subscribe();

            let handle = thread::spawn(move || {
                let mut prev = 100;
                while prev < 80000 {
                    prev = *block_on(reader.wait_for(|x| *x > prev)).0;

                    for i in 0..100 {
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
            guard.core.bump_version();

            drop(guard);
        }

        for t in threads {
            t.join().unwrap();
        }
    }
}
