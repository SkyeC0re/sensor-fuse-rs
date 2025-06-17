use std::{
    cell::UnsafeCell,
    hint::spin_loop,
    marker::PhantomPinned,
    mem::MaybeUninit,
    pin::Pin,
    ptr::{self, null_mut},
    sync::{
        MutexGuard,
        atomic::{AtomicPtr, AtomicU8, AtomicUsize, Ordering},
    },
    task::Waker,
};

use std::sync::Mutex;
const MAX_WAKE_CLUSTERING: usize = 8;

const VERSION_BUMP: usize = 2;
const CLOSED_BIT: usize = 0b1;

#[derive(Debug)]
#[repr(align(4))]
struct Node {
    waker: MaybeUninit<Waker>,
    next: AtomicPtr<Node>,
    prev: *mut Node,
    // ... WAKE_NEXT_BIT | COMPLETE_BIT
    state: AtomicU8,

    _p: PhantomPinned,
}

impl Node {
    const STATE_COMPLETE_BIT: u8 = 0b1;
    const STATE_WAKE_NEXT_BIT: u8 = 0b10;

    const SENTINEL: usize = 1;
}

pub struct Core {
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
}

impl Core {
    fn get_read_permits(&self, amount: usize) -> usize {
        let mut state = self.lock_state.load(Ordering::Relaxed);
        while state > amount {
            match self.lock_state.compare_exchange_weak(
                state,
                state - amount,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return amount,
                Err(new_state) => state = new_state,
            }

            spin_loop();
        }
        0
    }

    /// # Safety
    ///
    /// Must already hold a read permit.
    fn increase_read_permits(&self, amount: usize) {
        if self.lock_state.fetch_sub(amount, Ordering::Relaxed) < usize::MAX << 1 {
            panic!("Too many read permits");
        }
    }

    fn get_write_permit(&self) -> bool {
        self.lock_state
            .compare_exchange(usize::MAX, 0, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn notify_all(&self, mut owned_permits: usize) {
        let _ = self.version.fetch_add(VERSION_BUMP, Ordering::Relaxed);

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

        let _guard = self.main_queue_lock.lock().unwrap();
        unsafe {
            let pr_head = &mut *self.pr_head.get();

            if *pr_head == null_mut() {
                return;
            }

            *pr_head = null_mut();
            let pr_head = *pr_head;
            let pr_tail = self.pr_tail.swap(null_mut(), Ordering::AcqRel);

            let rw_tail = self.rw_tail.swap(pr_tail, Ordering::Acquire);
            if rw_tail != null_mut() {
                // Safety: Only the node's state can be accessed whilst we hold the mutex guard.
                (*rw_tail).next = AtomicPtr::new(pr_head);
                return;
            }

            // *self.rw_head.get() = null_mut();

            // let required_permits = if pr_head == pr_tail {
            //     1
            // } else {
            //     MAX_WAKE_CLUSTERING
            // };

            // if owned_permits < required_permits {
            //     owned_permits += self.get_read_permits(required_permits - owned_permits);

            //     if owned_permits == 0 {

            //     }
            // }

            // // Safety: Only the node's state can be accessed whilst we hold the mutex guard.
            // (*pr_tail).next = AtomicPtr::new(Node::SENTINEL as *mut _);

            // let mut waker_count = 0;
            // let wakers = [MaybeUninit::uninit(); MAX_WAKE_CLUSTERING];
            // let mut next = wc_head;
            // while waker_count < MAX_WAKE_CLUSTERING {
            //     *wakers.get_unchecked_mut(waker_count) =
            // }
        }
    }

    // Head should be zeroed
    fn wake_rw_queue_from(
        &self,
        _guard: MutexGuard<()>,
        node: *mut Node,
        mut known_tail: *mut Node,
        mut owned_permits: usize,
    ) {
        if owned_permits < MAX_WAKE_CLUSTERING {
            owned_permits += self.get_read_permits(MAX_WAKE_CLUSTERING - owned_permits);

            if owned_permits == 0 {
                unsafe {
                    *(self.rw_head.get()) = node;
                    (*node).prev = null_mut();
                    return;
                }
            }
        }

        let mut waker_count = 0;
        let mut wakers = [const { MaybeUninit::uninit() }; MAX_WAKE_CLUSTERING];
        let mut curr = node;

        while waker_count < owned_permits {
            unsafe {
                ptr::copy_nonoverlapping(&(*curr).waker, wakers.get_unchecked_mut(waker_count), 1);
                waker_count += 1;

                let mut next = (*curr).next.load(Ordering::Acquire);
                //  = ;
            }
        }
    }

    /// Insert a node into a queue via a standard procedure.
    unsafe fn insert_at_queue(queue: &AtomicPtr<Node>, elem: Pin<&mut Node>) -> *mut Node {
        unsafe {
            let elem: *mut Node = elem.get_unchecked_mut();
            let prev = queue.swap(elem, Ordering::AcqRel);

            if prev != null_mut() {
                (*prev).next.store(elem, Ordering::Release);
            }

            prev
        }
    }
}
