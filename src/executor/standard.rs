extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{ptr, task::Waker};
use parking_lot::Mutex;
use std::{cell::UnsafeCell, mem};

use crate::lock::DataWriteLock;

use super::{BoxedFn, ExecManager, ExecRegister};

struct Data<T> {
    callbacks_in: Vec<Box<dyn Send + FnMut(&T) -> bool>>,
    callbacks_out: Vec<Box<dyn Send + FnMut(&T) -> bool>>,
    wakers_in: Vec<Waker>,
    wakers_out: Vec<Waker>,
}

impl<T> Data<T> {
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            callbacks_in: Vec::new(),
            callbacks_out: Vec::new(),
            wakers_in: Vec::new(),
            wakers_out: Vec::new(),
        }
    }
}

/// Standard executor that supports registration of functions and wakers.
pub struct StdExec<T> {
    registration_mtx: Mutex<()>,
    data: UnsafeCell<Data<T>>,
}

unsafe impl<T> Sync for StdExec<T> where T: Send {}

/// TODO Optimize later
impl<T> StdExec<T> {
    /// Create a new Executor.
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            registration_mtx: Mutex::new(()),
            data: UnsafeCell::new(Data::new()),
        }
    }

    #[inline(always)]
    unsafe fn execute_unsafe<'a, L: DataWriteLock<Target = T>, F: FnOnce() -> L::WriteGuard<'a>>(
        &'a self,
        commit: F,
    ) where
        L: 'a,
    {
        let Data {
            callbacks_in,
            callbacks_out,
            wakers_in,
            wakers_out,
        } = &mut *self.data.get();

        let guard = self.registration_mtx.lock();
        for callback in callbacks_in.drain(..) {
            callbacks_out.push(callback);
        }
        mem::swap(wakers_in, wakers_out);
        let value = commit();
        drop(guard);
        // TODO Atomic downgrading causes significant slowdown.
        // let value = L::atomic_downgrade(value);

        for waker in (wakers_out).drain(..) {
            waker.wake();
        }

        let mut i = 0;
        let mut len = callbacks_out.len();
        let ptr_slice = callbacks_out.as_ptr().cast_mut();
        while i < len {
            let func = ptr_slice.add(i);
            if (*func)(&value) {
                i += 1;
            } else {
                len -= 1;
                ptr::swap(func, ptr_slice.add(len));
            }
        }

        callbacks_out.truncate(len);
    }
}

impl<T, L> ExecManager<L> for StdExec<T>
where
    L: DataWriteLock<Target = T>,
{
    fn execute<'a, C: FnOnce() -> L::WriteGuard<'a>>(&'a self, commit: C)
    where
        L: 'a,
    {
        unsafe {
            self.execute_unsafe::<L, C>(commit);
        }
    }
}

impl<T, F> ExecRegister<Box<F>> for StdExec<T>
where
    F: 'static + Send + FnMut(&T) -> bool,
{
    #[inline]
    fn register<C: FnOnce() -> Option<Box<F>>>(&self, condition: C) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(f) => {
                unsafe {
                    (*self.data.get()).callbacks_in.push(f);
                }
                true
            }
            None => false,
        };
        drop(guard);
        res
    }
}

impl<T> ExecRegister<BoxedFn<T>> for StdExec<T> {
    #[inline]
    fn register<C: FnOnce() -> Option<BoxedFn<T>>>(&self, condition: C) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(f) => {
                unsafe {
                    (*self.data.get()).callbacks_in.push(f);
                }
                true
            }
            None => false,
        };
        drop(guard);
        res
    }
}

impl<'a, T> ExecRegister<&'a Waker> for StdExec<T> {
    #[inline]
    fn register<C: FnOnce() -> Option<&'a Waker>>(&self, condition: C) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(w) => {
                unsafe {
                    (*self.data.get()).wakers_in.push(w.clone());
                }
                true
            }
            None => false,
        };
        drop(guard);
        res
    }
}
