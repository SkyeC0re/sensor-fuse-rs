use std::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    task::Waker,
};

use crate::lock::{DataReadLock, DataWriteLock, ReadGuardSpecifier};

pub mod standard;

pub trait CallbackExecute<T> {
    /// Wake all pending tasks and execute all registered callbacks. All callbacks that returned `false` are dropped.
    fn callback(&mut self, value: &T);
}

pub trait CallbackRegister<'req, F: 'req + FnMut(&T) -> bool, T> {
    /// Register a function on the callback manager's execution queue.
    fn register(&mut self, f: F);
}

pub trait WakerRegister {
    fn register_waker(&mut self, w: &Waker);
}

pub struct ExecData<T, E: CallbackExecute<T>> {
    pub(crate) exec_manager: UnsafeCell<E>,
    pub(crate) data: T,
}

/// Executor is always mutably borrowed and through an exlusive locking mechanism.
unsafe impl<T: Sync, E: CallbackExecute<T>> Sync for ExecData<T, E> {}

impl<T, E: CallbackExecute<T>> Deref for ExecData<T, E> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T, E: CallbackExecute<T>> DerefMut for ExecData<T, E> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

#[repr(transparent)]
pub struct ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    pub(crate) inner: L,
}

impl<L, T, E> ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    #[inline(always)]
    pub const fn new(lock: L) -> Self {
        Self { inner: lock }
    }
}

impl<L, T, E> Deref for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    type Target = L;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<L, T, E> DerefMut for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<L, T, E> ReadGuardSpecifier for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    type Target = T;

    type ReadGuard<'read> = ExecGuard<L::ReadGuard<'read>, T, E>   where
        Self: 'read;
}

impl<L, T, E> DataReadLock for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    #[inline]
    fn read(&self) -> Self::ReadGuard<'_> {
        ExecGuard {
            inner: self.inner.read(),
        }
    }

    #[inline]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.inner
            .try_read()
            .map(|guard| ExecGuard { inner: guard })
    }
}

impl<L, T, E> DataWriteLock for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    type WriteGuard<'write> = ExecGuardMut<L::WriteGuard<'write>, T, E>
    where
        Self: 'write;

    #[inline]
    fn write(&self) -> Self::WriteGuard<'_> {
        ExecGuardMut {
            inner: self.inner.write(),
        }
    }

    #[inline]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.inner
            .try_write()
            .map(|guard| ExecGuardMut { inner: guard })
    }
}

pub struct ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    pub(crate) inner: G,
}

impl<G, T, E> ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    #[inline(always)]
    pub const fn new(guard: G) -> Self {
        Self { inner: guard }
    }
}

impl<G, T, E> Deref for ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.inner.data
    }
}

pub struct ExecGuardMut<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    pub(crate) inner: G,
}

impl<G, T, E> ExecGuardMut<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    #[inline(always)]
    pub const fn new(guard: G) -> Self {
        Self { inner: guard }
    }
}

impl<G, T, E> Deref for ExecGuardMut<G, T, E>
where
    G: DerefMut<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.inner.data
    }
}

impl<G, T, E> DerefMut for ExecGuardMut<G, T, E>
where
    G: DerefMut<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.data
    }
}
