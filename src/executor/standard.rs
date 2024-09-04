extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{ptr, task::Waker};
use parking_lot::Mutex;

use super::{ExecManager, ExecManagerMut, ExecRegister, ExecRegisterMut};

pub struct ExecutorMut<T> {
    callbacks: Vec<Box<dyn Send + FnMut(&T) -> bool>>,
    wakers: Vec<Waker>,
}

impl<T> ExecutorMut<T> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            callbacks: Vec::new(),
            wakers: Vec::new(),
        }
    }
}

impl<T> ExecManagerMut<T> for ExecutorMut<T> {
    fn execute(&mut self, value: &T) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }

        let mut i = 0;
        let mut len = self.callbacks.len();
        let ptr_slice = self.callbacks.as_ptr().cast_mut();
        unsafe {
            while i < len {
                let func = ptr_slice.add(i);
                if (*func)(value) {
                    i += 1;
                } else {
                    len -= 1;
                    ptr::swap(func, ptr_slice.add(len));
                }
            }
        }

        self.callbacks.truncate(len);
    }
}

impl<T, F: 'static + Send + FnMut(&T) -> bool> ExecRegisterMut<F> for ExecutorMut<T> {
    #[inline]
    fn register(&mut self, f: F) {
        self.callbacks.push(Box::new(f));
    }
}

impl<T> ExecRegisterMut<&Waker> for ExecutorMut<T> {
    #[inline]
    fn register(&mut self, w: &Waker) {
        self.wakers.push(w.clone());
    }
}

#[repr(transparent)]
pub struct ExecutorImmut<T>(Mutex<ExecutorMut<T>>);

impl<T> ExecutorImmut<T> {
    #[inline]
    pub const fn new() -> Self {
        Self(Mutex::new(ExecutorMut::new()))
    }
}

impl<T> ExecManager<T> for ExecutorImmut<T> {
    #[inline]
    fn execute(&self, value: &T) {
        self.0.lock().execute(value);
    }
}

impl<T, F: 'static + Send + FnMut(&T) -> bool> ExecRegister<F> for ExecutorImmut<T> {
    #[inline]
    fn register(&self, f: F) {
        self.0.lock().register(f);
    }
}

impl<T> ExecRegister<&Waker> for ExecutorImmut<T> {
    #[inline]
    fn register(&self, w: &Waker) {
        self.0.lock().register(w);
    }
}
