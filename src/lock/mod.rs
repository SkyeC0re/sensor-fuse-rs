pub mod parking_lot;
#[cfg(feature = "std")]
pub mod std_sync;

use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};
use derived_deref::{Deref, DerefMut};
use std::{
    future::{poll_fn, Future},
    pin::pin,
};

pub trait ReadGuardSpecifier {
    /// The underlying type that the guard is protecting.
    type Target;

    /// A read guard with shared, immutable access to `type@Self::Target`.
    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;
}
pub trait DataReadLock: ReadGuardSpecifier {
    /// Provides at least immutable access to the current value inside the lock.
    fn read(&self) -> Self::ReadGuard<'_>;

    fn try_read(&self) -> Option<Self::ReadGuard<'_>>;
}
pub trait DataWriteLock: DataReadLock {
    /// A write guard with exclusive, mutable access to `type@Self::Target`.
    type WriteGuard<'write>: Deref<Target = Self::Target> + DerefMut
    where
        Self: 'write;
    /// A "downgraded" write guard. Downgraded is used loosely here, as what this represents is
    /// a guard that is mutually exclusive with other guards of this type and write guards, but not
    /// read guards. Functionally this means that for any locking mechanism this will either be
    /// a write guard (if the locking mechanism does not support atomic downgrades or shared access)
    /// or a read guard that was atomically downgraded from a write guard.
    type DowngradedGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;

    /// Provides mutable access to the current value inside the lock.
    fn write(&self) -> Self::WriteGuard<'_>;

    fn try_write(&self) -> Option<Self::WriteGuard<'_>>;

    /// Optional optimization for accessing the executor in callback enabled sensor writers and observers.
    /// This method should by default just return the write guard, but, if the lock allows for it,
    /// should return a read guard which was atomically downgraded from the given write guard.
    fn atomic_downgrade(write_guard: Self::WriteGuard<'_>) -> Self::DowngradedGuard<'_>;
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
    pub const fn new() -> Self {
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
