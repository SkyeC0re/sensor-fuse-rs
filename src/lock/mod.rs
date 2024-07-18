pub mod parking_lot;
pub mod std_sync;

use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use derived_deref::{Deref, DerefMut};

pub trait ReadGuardSpecifier {
    type Target;

    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self::Target: 'read,
        Self: 'read;
}

pub trait DataReadLock: ReadGuardSpecifier {
    /// Provides at least immutable access to the current value inside the lock.
    fn read(&self) -> Self::ReadGuard<'_>;

    fn try_read(&self) -> Option<Self::ReadGuard<'_>>;
}
pub trait DataWriteLock: DataReadLock {
    type WriteGuard<'write>: Deref<Target = Self::Target> + DerefMut
    where
        Self::Target: 'write,
        Self: 'write;

    /// Provides mutable access to the current value inside the lock.
    fn write(&self) -> Self::WriteGuard<'_>;

    fn try_write(&self) -> Option<Self::WriteGuard<'_>>;

    /// Downgrade mutable access to immutable access for a write guard originating from this lock instance.
    ///
    /// # Panics
    ///
    /// Behaviour is only well defined if and only if `write_guard` originates from `self`.
    /// May panic if `wirte_guard` does not originate from `self`. Implementations may also downgrade the guard
    /// regardless or return a new read guard originating from `self`.
    #[inline(always)]
    fn downgrade<'a>(&'a self, write_guard: Self::WriteGuard<'a>) -> Self::ReadGuard<'a> {
        drop(write_guard);
        self.read()
    }
    /// Upgrade immutable access to mutable access for a read guard from this specific lock instance.
    ///
    /// # Panics
    ///
    /// Behaviour is only well defined if and only if `read_guard` originates from `self`.
    /// May panic if `read_guard` does not originate from `self`. Implementations may also upgrade the guard
    /// regardless or return a new write guard originating from `self`.
    #[inline(always)]
    fn upgrade<'a>(&'a self, read_guard: Self::ReadGuard<'a>) -> Self::WriteGuard<'a> {
        drop(read_guard);
        self.write()
    }
}

pub trait DataLockFactory {
    type Lock: DataWriteLock;
    /// Contruct a new lock from the given initial value.
    fn new(init: <Self::Lock as ReadGuardSpecifier>::Target) -> Self::Lock;
}

#[derive(Deref, DerefMut, Clone)]
#[repr(transparent)]
pub struct FalseReadLock<T>(pub T);

impl<T> ReadGuardSpecifier for FalseReadLock<T> {
    type Target = T;

    type ReadGuard<'read> = &'read T where T: 'read;
}

impl<T> DataReadLock for FalseReadLock<T> {
    fn read(&self) -> Self::ReadGuard<'_> {
        self
    }

    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        Some(self)
    }
}

#[derive(Default, Clone, Copy)]
pub struct OwnedFalseLock<T>(PhantomData<T>);

impl<T> OwnedFalseLock<T> {
    #[inline(always)]
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> ReadGuardSpecifier for OwnedFalseLock<T> {
    type Target = T;

    type ReadGuard<'read> = OwnedData<T> where T: 'read;
}

#[derive(Deref, DerefMut, Clone)]
#[repr(transparent)]
pub struct OwnedData<T>(pub T);

impl<T> OwnedData<T> {
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.0
    }
}
