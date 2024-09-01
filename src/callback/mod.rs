pub mod vec_box;

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
};

use crate::lock::{DataReadLock, DataWriteLock, ReadGuardSpecifier};

pub trait CallbackExecute<T> {
    /// Wake all pending tasks and execute all registered callbacks. All callbacks that returned `false` are dropped.
    fn callback(&mut self, value: &T);
}

impl<T> CallbackExecute<T> for () {
    #[inline(always)]
    fn callback(&mut self, _: &T) {}
}

pub trait ExecRegister<F> {
    /// Register an executable unit on the callback manager's execution set. In most cases this will usually be a function.
    fn register(&mut self, f: F);
}

pub struct ExecData<T, E: CallbackExecute<T>> {
    pub(crate) data: T,
    pub(crate) exec_manager: UnsafeCell<E>,
}

/// Executor is always mutably borrowed and through an exlusive locking mechanism.
unsafe impl<T, E: CallbackExecute<T>> Sync for ExecData<T, E> where T: Sync {}

impl<T, E: CallbackExecute<T>> ExecData<T, E> {
    #[inline(always)]
    pub(crate) const fn new(data: T, exec_manager: E) -> Self {
        Self {
            exec_manager: UnsafeCell::new(exec_manager),
            data,
        }
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
    pub(crate) const fn new(lock: L) -> Self {
        Self { inner: lock }
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
        ExecGuard::new(self.inner.read())
    }

    #[inline]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.inner.try_read().map(|guard| ExecGuard::new(guard))
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
        ExecGuardMut::new(self.inner.write())
    }

    #[inline]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.inner.try_write().map(|guard| ExecGuardMut::new(guard))
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
