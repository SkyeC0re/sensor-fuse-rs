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
    ops::{BitXor, Deref},
    sync::atomic::AtomicU8,
};

use async_lock::{
    futures::{Read, Write},
    RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use either::Either;

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

const NODE_COMPLETE_BIT: u8 = 8;
const NODE_CANCEL_BIT: u8 = 4;
const NODE_DROP_BIT: u8 = 2;
const NODE_LOCK_BIT: u8 = 1;

#[repr(align(4))]
struct Node {
    waker: MaybeUninit<Waker>,
    // PTR | IS_LAST_BIT | TYPE_BIT
    next: AtomicUsize,
    // ... | COMPLETE_BIT | CANCEL_BIT | DROP_BIT | LOCK_BIT
    state: AtomicU8,
}

impl Node {
    /// Attempts to recycle an old allocation or discard it an create a new allocation if that fails.
    /// If `None` is returned for the node, the waker was reset inside the queue, otherwise exlusive ownership is guaranteed
    /// over the (potentially newly allocated) Node.
    ///
    /// # Safety
    ///
    /// It can be safely assumed that `None` can only ever be returned if `allow_in_queue_reset` is true.
    #[inline(always)]
    unsafe fn reuse_or_realloc(
        node: *mut UnsafeCell<Self>,
        waker: Waker,
        allow_in_queue_reset: bool,
    ) -> Option<&'static mut UnsafeCell<Self>> {
        let old_node = &*(*node).get();
        let mut state = old_node.state.load(Ordering::Acquire);
        if state & NODE_COMPLETE_BIT != 0 {
            // Free to re-use mutably. We are the sole owners.
            Node::assume_exclusive_reset(node, waker);
            return Some(&mut *node);
        }

        state = old_node.state.fetch_or(NODE_LOCK_BIT, Ordering::AcqRel);

        if state & NODE_LOCK_BIT != 0 {
            while state & NODE_LOCK_BIT != 0 {
                spin_loop();
                state = old_node.state.load(Ordering::Acquire);
            }

            debug_assert_ne!(state & NODE_COMPLETE_BIT, 0);

            // Free to re-use mutably. We are the sole owners.
            Node::assume_exclusive_reset(node, waker);
            return Some(&mut *node);
        }

        if allow_in_queue_reset {
            // Piggy back off of the fact that the node is still in an appropriate queue.

            debug_assert_eq!(state, NODE_LOCK_BIT | NODE_CANCEL_BIT);

            let _ = (*node).get_mut().waker.write(waker);
            old_node.state.store(0, Ordering::Release);

            return None;
        }

        // No attempts at recycling the allocation succeeded. Dump it and acquire a new one.
        old_node
            .state
            .store(state ^ (NODE_LOCK_BIT | NODE_DROP_BIT), Ordering::Release);

        let node = Box::new(UnsafeCell::new(Node {
            waker: MaybeUninit::new(waker),
            next: AtomicUsize::new(0),
            state: AtomicU8::new(0),
        }));

        return Some(unsafe { &mut *Box::into_raw(node) });
    }

    /// Reset the node's values for a new future that is to be added to a queue.
    ///
    /// # Safety
    ///
    /// Behaviour is undefined if node is not in a completed (i.e. exclusive) state.
    #[inline(always)]
    unsafe fn assume_exclusive_reset(node: *mut UnsafeCell<Self>, waker: Waker) {
        let node = &mut *node;
        let _ = node.get_mut().waker.write(waker);
        *node.get_mut().next.get_mut() = 0;
        *node.get_mut().state.get_mut() = 0;
    }

    /// Cancels and drops the waker if the node has not been completed yet.
    ///
    /// Returns whether a complete bit was detected.
    ///
    /// # Safety
    ///
    /// It is undefined behaviour to call this twice on the same node without resetting it.
    #[inline(always)]
    unsafe fn cancel(node: *mut UnsafeCell<Self>) -> bool {
        let node = (*node).get_mut();

        let mut state = node.state.load(Ordering::Acquire);
        debug_assert_eq!(state & NODE_CANCEL_BIT, 0);

        if state & (NODE_COMPLETE_BIT) != 0 {
            // We have already been completed. Nothing to do.
            return true;
        }

        state = node.state.fetch_or(NODE_LOCK_BIT, Ordering::AcqRel);
        if state & NODE_LOCK_BIT != 0 {
            // We are about to be completed. Nothing to do.
            return true;
        }

        debug_assert_eq!(state, NODE_LOCK_BIT);

        // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
        // of dropping the waker.
        let waker = node.waker.assume_init_read();
        node.state
            .store(state ^ (NODE_LOCK_BIT | NODE_CANCEL_BIT), Ordering::Release);
        drop(waker);

        return false;
    }

    /// Wake the next element in the wait changed queue, skipping over cancelled nodes and appropriately dropping
    /// drop requested nodes.
    ///
    /// # Safety
    ///
    /// It is undefined behaviour to call this before a node is completed (or being completed) or to call this twice for
    /// the same node for the same completion.
    #[inline(always)]
    unsafe fn wake_next_wait_changed(mut node: *mut UnsafeCell<Self>) {
        let mut next_ptr;
        loop {
            next_ptr = (*node).get_mut().next.load(Ordering::Acquire);

            if next_ptr > 0 {
                if next_ptr == 1 {
                    return;
                } else {
                    break;
                }
            }

            spin_loop();
        }

        node = next_ptr as *mut _;

        loop {
            next_ptr = (*node).get_mut().next.load(Ordering::Acquire);

            if next_ptr == 0 {
                spin_loop();
                continue;
            }

            // Responsibility was either shifted successfully or we have reached the end of the queue.
            if Node::complete_and_wake(node) || next_ptr == 1 {
                return;
            }

            node = next_ptr as *mut _;
        }
    }

    /// Attempt to complete wake this node if possible, and returns true if it was successfully completed and awoken.
    ///
    /// # Safety
    ///
    /// Should only be called on a node that is in a queue and has not been marked as completed (i.e. not previously cancelled or dropped).
    /// After this call completes it is undefined behaviour to access the node again.
    #[inline(always)]
    unsafe fn complete_and_wake(node: *mut UnsafeCell<Self>) -> bool {
        let mut state;
        loop {
            state = (*node)
                .get_mut()
                .state
                .fetch_or(NODE_LOCK_BIT, Ordering::AcqRel);

            if state & NODE_LOCK_BIT == 0 {
                break;
            }

            spin_loop();
        }

        if state == NODE_DROP_BIT {
            drop(Box::from_raw(node));
            return false;
        } else if state == NODE_CANCEL_BIT {
            (*node).get_mut().state.store(
                state ^ (NODE_LOCK_BIT | NODE_COMPLETE_BIT),
                Ordering::Release,
            );
            return false;
        }

        debug_assert_eq!(state, 0);

        // Get waker first with a cheap, bit level copy, allow other threads to continue and then spend the potential cost
        // of waking.
        let waker = (*node).get_mut().waker.assume_init_read();
        (*node).get_mut().state.store(
            state ^ (NODE_LOCK_BIT | NODE_COMPLETE_BIT),
            Ordering::Release,
        );
        waker.wake();

        true
    }
}

struct MySensorCore<T> {
    queue_head: AtomicUsize,
    queue_tail: AtomicUsize,
    idle_reads_head: AtomicPtr<UnsafeCell<Node>>,
    idle_reads_tail: AtomicPtr<UnsafeCell<Node>>,
    change_waiters_head: AtomicPtr<UnsafeCell<Node>>,
    num_writers: AtomicUsize,
    version: AtomicUsize,
    data: UnsafeCell<T>,
}

impl<T> MySensorCore<T> {
    #[inline(always)]
    fn version(&self) -> Version {
        // Relaxed could work?
        Version(self.version.load(Ordering::Acquire))
    }
}

const SA_TYPE_FREE: usize = 0;
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

struct MyWaitChangedFut<'a, T> {
    observer_data: &'a mut ObserverData,
    core: &'a MySensorCore<T>,
    init: bool,
}

impl<'a, T> MyWaitChangedFut<'a, T> {
    /// Wake the next future in the queue, if any.
    #[inline(always)]
    unsafe fn wake_next(node: *mut UnsafeCell<Node>) {}
}

impl<'a, T> Future for MyWaitChangedFut<'a, T> {
    type Output = SymResult<Version>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let node = (self.observer_data.static_area & SA_MASK_PTR) as *mut UnsafeCell<Node>;
        if self.init {
            if unsafe { (*node).get_mut().state.load(Ordering::Acquire) & NODE_COMPLETE_BIT == 0 } {
                return Poll::Pending;
            }

            let version = self.core.version();
            if self.observer_data.version != version {
                // Do not rely on `Drop` trait to wake the next element. It may not be instantaneous.
                unsafe {
                    if Node::cancel(node) {
                        Node::wake_next_wait_changed(node);
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
            let prev_head = self.core.change_waiters_head.swap(node, Ordering::AcqRel);

            // Safety: No data initialization depends on the ordering of this.
            node.get_mut()
                .next
                .store(prev_head as usize, Ordering::Relaxed);

            let node: *mut UnsafeCell<Node> = node;
            self.observer_data.static_area = (node as usize) | SA_TYPE_WAIT_CHANGED;
        }

        self.init = true;

        Poll::Pending
    }
}

impl<'a, T> Drop for MyWaitChangedFut<'a, T> {
    fn drop(&mut self) {
        if self.init {
            let node = (self.observer_data.static_area & SA_MASK_PTR) as _;
            unsafe {
                if Node::cancel(node) {
                    Node::wake_next_wait_changed(node);
                }
            }
        }
    }
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
