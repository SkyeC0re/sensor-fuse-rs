use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use crate::lock::{DataReadLock, DataWriteLock, ReadGuardSpecifier};

pub mod standard;

pub trait CallbackManager {
    type Target;

    fn register<F: 'static + FnMut(&Self::Target) -> bool>(&mut self, f: F);
    fn callback(&mut self, value: &Self::Target);
}

pub struct ExecData<T, E: CallbackManager> {
    pub(crate) exec_manager: UnsafeCell<E>,
    pub(crate) data: T,
}

impl<T, E: CallbackManager> Deref for ExecData<T, E> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T, E: CallbackManager> DerefMut for ExecData<T, E> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

#[repr(transparent)]
pub struct ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    inner: L,
}

impl<L, T, E> ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    #[inline(always)]
    pub const fn new(lock: L) -> Self {
        Self { inner: lock }
    }
}

impl<L, T, E> Deref for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type Target = L;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<L, T, E> DerefMut for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<L, T, E> ReadGuardSpecifier for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type Target = T;

    type ReadGuard<'read> = ExecGuard<L::ReadGuard<'read>, T, E>   where
        Self::Target: 'read,
        Self: 'read;
}

impl<L, T, E> DataReadLock for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    fn read(&self) -> Self::ReadGuard<'_> {
        ExecGuard {
            inner: self.inner.read(),
            _types: PhantomData,
        }
    }

    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.inner.try_read().map(|guard| ExecGuard {
            inner: guard,
            _types: PhantomData,
        })
    }
}

impl<L, T, E> DataWriteLock for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type WriteGuard<'write> = ExecGuardMut<L::WriteGuard<'write>, T, E>
    where
        Self::Target: 'write,
        Self: 'write;

    fn write(&self) -> Self::WriteGuard<'_> {
        ExecGuardMut {
            inner: self.inner.write(),
            _types: PhantomData,
        }
    }

    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.inner.try_write().map(|guard| ExecGuardMut {
            inner: guard,
            _types: PhantomData,
        })
    }
}

pub struct ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    pub(crate) inner: G,
    _types: PhantomData<(T, E)>,
}

impl<G, T, E> ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    #[inline(always)]
    pub const fn new(guard: G) -> Self {
        Self {
            inner: guard,
            _types: PhantomData,
        }
    }
}

impl<G, T, E> Deref for ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.data
    }
}

pub struct ExecGuardMut<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    pub(crate) inner: G,
    _types: PhantomData<(T, E)>,
}

impl<G, T, E> ExecGuardMut<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    #[inline(always)]
    pub const fn new(guard: G) -> Self {
        Self {
            inner: guard,
            _types: PhantomData,
        }
    }
}

impl<G, T, E> Deref for ExecGuardMut<G, T, E>
where
    G: DerefMut<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.data
    }
}

impl<G, T, E> DerefMut for ExecGuardMut<G, T, E>
where
    G: DerefMut<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.data
    }
}
