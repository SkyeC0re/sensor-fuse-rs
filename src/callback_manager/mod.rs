use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    task::Waker,
};

use crate::lock::{DataReadLock, DataWriteLock, ReadGuardSpecifier};

pub mod standard;

pub trait CallbackManager<T> {
    /// Register a function on the callback manager's execution queue.
    fn register<F: 'static + FnMut(&T) -> bool>(&mut self, f: F);

    /// Register a waker on the callback manager.
    /// By default any callback manager supports an unoptimized version of handling wakers as a oneshot function.
    fn register_waker(&mut self, w: &Waker) {
        let mut waker_clone = Some(w.clone());
        self.register(move |_| {
            if let Some(waker) = waker_clone.take() {
                waker.wake();
            }
            false
        });
    }
    /// Execute all callbacks registered on the manager and drop all callbacks that returns `false`.
    fn callback(&mut self, value: &T);
}

pub struct ExecData<T, E: CallbackManager<T>> {
    pub(crate) exec_manager: UnsafeCell<E>,
    pub(crate) data: T,
}

impl<T, E: CallbackManager<T>> Deref for ExecData<T, E> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T, E: CallbackManager<T>> DerefMut for ExecData<T, E> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

#[repr(transparent)]
pub struct ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager<T>,
{
    inner: L,
}

impl<L, T, E> ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager<T>,
{
    #[inline(always)]
    pub const fn new(lock: L) -> Self {
        Self { inner: lock }
    }
}

impl<L, T, E> Deref for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager<T>,
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
    E: CallbackManager<T>,
{
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<L, T, E> ReadGuardSpecifier for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager<T>,
{
    type Target = T;

    type ReadGuard<'read> = ExecGuard<L::ReadGuard<'read>, T, E>   where
        Self::Target: 'read,
        Self: 'read;
}

impl<L, T, E> DataReadLock for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager<T>,
{
    #[inline]
    fn read(&self) -> Self::ReadGuard<'_> {
        ExecGuard {
            inner: self.inner.read(),
            _types: PhantomData,
        }
    }

    #[inline]
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
    E: CallbackManager<T>,
{
    type WriteGuard<'write> = ExecGuardMut<L::WriteGuard<'write>, T, E>
    where
        Self::Target: 'write,
        Self: 'write;

    #[inline]
    fn write(&self) -> Self::WriteGuard<'_> {
        ExecGuardMut {
            inner: self.inner.write(),
            _types: PhantomData,
        }
    }

    #[inline]
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
    E: CallbackManager<T>,
{
    pub(crate) inner: G,
    _types: PhantomData<(T, E)>,
}

impl<G, T, E> ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager<T>,
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
    E: CallbackManager<T>,
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
    E: CallbackManager<T>,
{
    pub(crate) inner: G,
    _types: PhantomData<(T, E)>,
}

impl<G, T, E> ExecGuardMut<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager<T>,
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
    E: CallbackManager<T>,
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
    E: CallbackManager<T>,
{
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.data
    }
}
