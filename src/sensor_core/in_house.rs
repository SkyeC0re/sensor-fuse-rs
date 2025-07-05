use std::{
    cell::UnsafeCell,
    hint::spin_loop,
    marker::PhantomPinned,
    mem::MaybeUninit,
    num::NonZero,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::{self, null_mut},
    sync::{
        MutexGuard,
        atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering},
    },
    task::{Poll, Waker},
};

use std::sync::Mutex;

use crate::{SensorObserve, SensorWrite, ShareStrategy};
const MAX_WAKE_CLUSTERING: usize = 8;

const WRITE_PERMIT_VALUE: usize = usize::MAX;
const MAX_READ_PERMITS: usize = WRITE_PERMIT_VALUE >> 1;

const VERSION_BUMP: usize = 2;
const CLOSED_BIT: usize = 0b1;

#[derive(Debug)]
#[repr(align(4))]
struct Node {
    waker: MaybeUninit<Waker>,
    next: AtomicPtr<Node>,
    prev: *mut Node,
    // ... | WAKE_NEXT_BIT | COMPLETE_BIT
    state: AtomicU8,
    is_write: bool,
    _p: PhantomPinned,
}

impl Node {
    // Safety: We rely on this value to be 1.
    const STATE_COMPLETE_BIT: u8 = 0b1;
    const STATE_WAKE_NEXT_BIT: u8 = 0b10;

    const SENTINEL: usize = 1;
}

pub struct Core<T> {
    // Permits available
    lock_state: AtomicUsize,
    version: AtomicUsize,
    main_queue_lock: Mutex<()>,
    rw_head: UnsafeCell<*mut Node>,
    rw_tail: AtomicPtr<Node>,
    pr_head: UnsafeCell<*mut Node>,
    pr_tail: AtomicPtr<Node>,

    writes_queued: AtomicUsize,
    writers: AtomicUsize,

    changed_queue_lock: Mutex<()>,
    wc_head: AtomicPtr<Node>,

    data: UnsafeCell<T>,
}

impl<T> Core<T> {
    fn try_get_read_permits(&self, amount: usize) -> bool {
        let mut outstanding_permits = self.lock_state.load(Ordering::Relaxed);
        while amount <= MAX_READ_PERMITS.saturating_sub(outstanding_permits) {
            match self.lock_state.compare_exchange_weak(
                outstanding_permits,
                outstanding_permits + amount,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(new_state) => outstanding_permits = new_state,
            }

            spin_loop();
        }
        false
    }

    /// Releases permits and returns whether or this release represents
    /// the last permits being released.
    fn release_permits(&self, amount: usize) -> bool {
        self.lock_state.fetch_sub(amount, Ordering::Release) == amount
    }

    /// # Safety
    ///
    /// Must already hold a read permit.
    fn increase_read_permits(&self, amount: usize) {
        if self.lock_state.fetch_add(amount, Ordering::Relaxed) > MAX_READ_PERMITS {
            panic!("Too many read permits");
        }
    }

    /// Attempt to upgrade a set of read permits into a write permit.
    fn try_upgrade_permits(&self, permits: usize) -> bool {
        self.lock_state
            .compare_exchange(
                permits,
                WRITE_PERMIT_VALUE,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    fn release_write_permit(&self) {
        self.lock_state.store(0, Ordering::Release);
    }

    fn try_read(&self) -> Option<ReadGuard<T>> {
        if self.writes_queued.load(Ordering::Acquire) != 0 {
            return None;
        }

        if !self.try_get_read_permits(1) {
            return None;
        }

        Some(ReadGuard { core: self })
    }

    unsafe fn notify_all(&self, mut owned_permits: usize) {
        let _ = self.version.fetch_add(VERSION_BUMP, Ordering::Relaxed);

        // `AcqRel`, because the next wait changed element that inserts itself should be made aware
        // of the version bump that just ocurred,
        let wc_head = self.wc_head.swap(null_mut(), Ordering::AcqRel);
        if wc_head != null_mut() {
            unsafe {
                let waker = (*wc_head).waker.assume_init_read();
                (*wc_head).state.store(
                    Node::STATE_COMPLETE_BIT | Node::STATE_WAKE_NEXT_BIT,
                    Ordering::Release,
                );
                waker.wake();
            }
        }

        let guard = self.main_queue_lock.lock().unwrap();
        unsafe {
            let pr_head = &mut *self.pr_head.get();

            if *pr_head == null_mut() {
                return;
            }
            *pr_head = null_mut();

            let pr_head = *pr_head;
            let pr_tail = self.pr_tail.swap(null_mut(), Ordering::Acquire);
            let rw_tail = self.rw_tail.swap(pr_tail, Ordering::Acquire);
            if rw_tail != null_mut() {
                // Safety: Only the node's state can be accessed whilst we hold the mutex guard.
                (*rw_tail).next = AtomicPtr::new(pr_head);
                return;
            }

            // See if we can wake immediately
            if owned_permits == 0 {
                owned_permits += match self.try_get_read_permits(MAX_WAKE_CLUSTERING) {
                    true => MAX_WAKE_CLUSTERING,
                    false => 0,
                };
            }

            if owned_permits == 0 {
                *self.rw_head.get() = pr_head;
                return;
            }

            self.wake_next_read(pr_head, guard, owned_permits);
        }
    }

    // Wake the next waiting cluster of reads from the current wake set.
    //
    // # Safety
    //
    // Head should be zeroed. Permits should either be non-zero, or the caller must be holding on to a permit that it is not giving away.
    unsafe fn wake_next_read(
        &self,
        // The first read in the cluster.
        node: *mut Node,
        // Main queue lock guard.
        guard: MutexGuard<()>,
        // Read permits that are being given away to this function.
        mut permits: usize,
    ) {
        unsafe {
            if permits < MAX_WAKE_CLUSTERING {
                self.increase_read_permits(MAX_WAKE_CLUSTERING - permits);
                permits = MAX_WAKE_CLUSTERING;

                if permits == 0 {
                    *(self.rw_head.get()) = node;
                    (*node).prev = null_mut();
                    return;
                }
            }

            let mut cluster_size = 0;
            let mut wakers = [const { MaybeUninit::uninit() }; MAX_WAKE_CLUSTERING];
            let mut curr = node;
            loop {
                ptr::copy_nonoverlapping(&(*curr).waker, wakers.get_unchecked_mut(cluster_size), 1);
                cluster_size += 1;

                if cluster_size == MAX_WAKE_CLUSTERING {
                    (*curr).state.store(
                        Node::STATE_COMPLETE_BIT | Node::STATE_WAKE_NEXT_BIT,
                        Ordering::Release,
                    );
                    break;
                }

                (*curr)
                    .state
                    .store(Node::STATE_COMPLETE_BIT, Ordering::Release);

                let mut next = (*curr).next.load(Ordering::Acquire);
                if next == null_mut() {
                    if self
                        .rw_tail
                        .compare_exchange(curr, null_mut(), Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        break;
                    }

                    loop {
                        spin_loop();
                        next = (*curr).next.load(Ordering::Acquire);
                        if next != null_mut() {
                            break;
                        }
                    }
                }

                if (*next).is_write {
                    *self.rw_head.get() = next;
                    break;
                }

                curr = next;
            }

            drop(guard);

            let releasable_permits = permits - cluster_size;
            if releasable_permits > 0 {
                let _ = self.release_permits(releasable_permits);
            }

            for waker in wakers.get_unchecked((cluster_size - 1)..=0) {
                waker.assume_init_read().wake();
            }
        }
    }

    fn wake_next_wait_changed(
        &self,
        node: *mut Node,
        // Changed lock guard.
        guard: MutexGuard<()>,
    ) {
        unsafe {
            let mut cluster_size = 0;
            let mut wakers = [const { MaybeUninit::uninit() }; MAX_WAKE_CLUSTERING];
            let mut curr = node;
            loop {
                ptr::copy_nonoverlapping(&(*curr).waker, wakers.get_unchecked_mut(cluster_size), 1);
                cluster_size += 1;

                if cluster_size == MAX_WAKE_CLUSTERING {
                    (*curr).state.store(
                        Node::STATE_COMPLETE_BIT | Node::STATE_WAKE_NEXT_BIT,
                        Ordering::Release,
                    );
                    break;
                }

                (*curr)
                    .state
                    .store(Node::STATE_COMPLETE_BIT, Ordering::Release);

                let mut next;
                loop {
                    next = (*curr).next.load(Ordering::Acquire);

                    if next != null_mut() {
                        break;
                    }
                    spin_loop();
                }

                if next == Node::SENTINEL as _ {
                    break;
                }

                curr = next;
            }

            drop(guard);
            for waker in wakers.get_unchecked((cluster_size - 1)..=0) {
                waker.assume_init_read().wake();
            }
        }
    }

    fn wake_rw_queue(&self, guard: MutexGuard<()>, mut permits: usize) {
        unsafe {
            let node = *self.rw_head.get();
            if node == null_mut() {
                return;
            }

            if !(*node).is_write {
                // Ensure we meet the safety conditions of `wake_next_read`.
                if permits == 0 {
                    if !self.try_get_read_permits(MAX_WAKE_CLUSTERING) {
                        return;
                    }
                    permits = MAX_WAKE_CLUSTERING;
                }

                *self.rw_head.get() = null_mut();
                self.wake_next_read(node, guard, permits);
                return;
            }

            if permits < WRITE_PERMIT_VALUE && !self.try_upgrade_permits(permits) {
                return;
            }

            let mut next = (*node).next.load(Ordering::Relaxed);

            if next == null_mut() {
                if self
                    .rw_tail
                    .compare_exchange(node, null_mut(), Ordering::Relaxed, Ordering::Acquire)
                    .is_err()
                {
                    loop {
                        next = self.rw_tail.load(Ordering::Acquire);
                        if next != null_mut() {
                            break;
                        }
                        spin_loop();
                    }
                }
            }
            *self.rw_head.get() = next;

            (*node)
                .state
                .store(Node::STATE_COMPLETE_BIT, Ordering::Release);
        }
    }
}

pub struct WriteGuard<'a, T> {
    core: &'a Core<T>,
}

impl<'a, T> Deref for WriteGuard<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.core.data.get() }
    }
}

impl<'a, T> DerefMut for WriteGuard<'a, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.core.data.get() }
    }
}

impl<'a, T> Drop for WriteGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        let guard = self.core.main_queue_lock.lock().unwrap();
        self.core.wake_rw_queue(guard, WRITE_PERMIT_VALUE);
    }
}

pub struct ReadGuard<'a, T> {
    core: &'a Core<T>,
}

impl<'a, T> Deref for ReadGuard<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.core.data.get() }
    }
}

impl<'a, T> Drop for ReadGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        if self.core.release_permits(1) {
            let guard = self.core.main_queue_lock.lock().unwrap();
            self.core.wake_rw_queue(guard, 0);
        }
    }
}

#[repr(transparent)]
pub struct Writer<T, R: ShareStrategy<Target = Core<T>>> {
    core: R,
}

impl<T, R: ShareStrategy<Target = Core<T>>> SensorWrite for Writer<T, R> {
    type Target = T;

    type WriteGuard<'a>
        = WriteGuard<'a, T>
    where
        Self: 'a;

    fn notify_all(&self) {
        unsafe { self.core.notify_all(0) };
    }

    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        if self.core.writes_queued.load(Ordering::Relaxed) != 0 {
            return None;
        }

        if !self.core.try_upgrade_permits(0) {
            return None;
        }

        Some(WriteGuard { core: &self.core })
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
        = ReadGuard<'read, T>
    where
        Self: 'read;

    #[inline]
    fn mark_seen(&mut self) {
        self.version = self.core.version.load(Ordering::Relaxed);
    }

    #[inline]
    fn mark_unseen(&mut self) {
        self.version = self
            .core
            .version
            .load(Ordering::Relaxed)
            .wrapping_sub(VERSION_BUMP);
    }

    #[inline]
    fn has_changed(&self) -> bool {
        self.version != self.core.version.load(Ordering::Relaxed)
    }

    #[inline]
    fn is_closed(&self) -> bool {
        self.core.version.load(Ordering::Relaxed) & CLOSED_BIT != 0
    }
}

struct Read<'a, T> {
    core: &'a Core<T>,
    node: Option<UnsafeCell<Node>>,
}

impl<'a, T> Future for Read<'a, T> {
    type Output = ReadGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let s = self.get_unchecked_mut();

            if let Some(node) = &s.node {
                let node = node.get();
                let state = (*node).state.load(Ordering::Acquire);
                if state & Node::STATE_COMPLETE_BIT == 0 {
                    return Poll::Pending;
                }

                let read_guard = ReadGuard { core: s.core };
                if state & Node::STATE_WAKE_NEXT_BIT == 0 {
                    return Poll::Ready(read_guard);
                }

                let queue_guard = s.core.main_queue_lock.lock().unwrap();
                let mut next = (*node).next.load(Ordering::Acquire);
                if next == null_mut() {
                    match s.core.rw_tail.compare_exchange(
                        node,
                        null_mut(),
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return Poll::Ready(read_guard),
                        Err(new_next) => next = new_next,
                    }
                }
                s.core.wake_next_read(next, queue_guard, 0);

                s.node = None;
                return Poll::Ready(read_guard);
            }

            if let Some(guard) = s.core.try_read() {
                return Poll::Ready(guard);
            }

            s.node = Some(UnsafeCell::new(Node {
                waker: MaybeUninit::new(cx.waker().clone()),
                next: AtomicPtr::new(null_mut()),
                prev: null_mut(),
                state: AtomicU8::new(0),
                is_write: false,
                _p: PhantomPinned,
            }));

            let node = s.node.as_mut().unwrap_unchecked().get();

            let tail = s.core.rw_tail.swap(node, Ordering::AcqRel);
            if tail != null_mut() {
                (*node).prev = tail;
                (*tail).next.store(node, Ordering::Release);
                return Poll::Pending;
            }

            let queue_guard = s.core.main_queue_lock.lock().unwrap();

            if let Some(read_guard) = s.core.try_read() {
                let mut next = (*node).next.load(Ordering::Acquire);
                if next == null_mut() {
                    match s.core.rw_tail.compare_exchange(
                        node,
                        null_mut(),
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return Poll::Ready(read_guard),
                        Err(new_next) => next = new_next,
                    }
                }
                s.core.wake_next_read(next, queue_guard, 0);
                (*node).waker.assume_init_drop();
                s.node = None;
                return Poll::Ready(read_guard);
            }

            *s.core.rw_head.get() = node;
            Poll::Pending
        }
    }
}

impl<'a, T> Drop for Read<'a, T> {
    fn drop(&mut self) {
        unsafe {
            let node = if let Some(node) = &self.node {
                node.get()
            } else {
                return;
            };

            let queue_guard = self.core.main_queue_lock.lock().unwrap();

            let mut next = (*node).next.load(Ordering::Acquire);
            if next == null_mut() {
                if let Err(new_next) = self.core.rw_tail.compare_exchange(
                    node,
                    null_mut(),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    next = new_next;
                }
            }

            let state = *(*node).state.get_mut();
            if state == 0 {
                if next != null_mut() {
                    (*next).prev = (*node).prev;
                }

                let prev = (*node).prev;
                if prev == null_mut() {
                    *self.core.rw_head.get() = null_mut();
                } else {
                    // Safety: we have exclusive access to `prev.next`, the owner of that
                    // node will not touch it until it has a queue lock.
                    (*prev).next = AtomicPtr::new(next);
                }

                drop(queue_guard);
                (*node).waker.assume_init_drop();
                return;
            }

            if (state & Node::STATE_WAKE_NEXT_BIT == 0) {
                // Indpendent drop of readguard occurs here, but we already have a queue lock,
                // so we can unconditionally try to wake other elements in the queue.
                self.core.wake_rw_queue(queue_guard, 1);
                return;
            }

            if next == null_mut() || (*next).is_write {
                return;
            }

            self.core.wake_next_read(next, queue_guard, 1);
        }
    }
}
