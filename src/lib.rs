pub mod parking_lot;
pub mod std_sync;

use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use derived_deref::{Deref, DerefMut};
use parking_lot::FusedSensorObserver;

pub trait DataLock: DataReadLock {
    type WriteGuard<'write>: Deref<Target = Self::Target> + DerefMut
    where
        Self::Target: 'write,
        Self: 'write;

    /// Provides mutable access to the current value inside the lock.
    fn write(&self) -> Self::WriteGuard<'_>;

    /// Downgrade mutable access to immutable access for a write guard originating from this lock instance.
    ///
    /// # Panics
    ///
    /// Behaviour is only well defined if and only if `write_guard` originates from self.
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
    /// Behaviour is only well defined if and only if `read_guard` originates from self.
    /// May panic if `read_guard` does not originate from `self`. Implementations may also upgrade the guard
    /// regardless or return a new write guard originating from `self`.
    #[inline(always)]
    fn upgrade<'a>(&'a self, read_guard: Self::ReadGuard<'a>) -> Self::WriteGuard<'a> {
        drop(read_guard);
        self.write()
    }
}

pub trait DataReadLock {
    type Target;

    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self::Target: 'read,
        Self: 'read;

    /// Provides at least immutable access to the current value inside the lock.
    fn read(&self) -> Self::ReadGuard<'_>;
}

#[derive(Deref, DerefMut)]
pub struct FalseReadLock<T>(T);

impl<T> DataReadLock for FalseReadLock<T> {
    type Target = T;

    type ReadGuard<'read> = &'read T where T: 'read;

    fn read(&self) -> Self::ReadGuard<'_> {
        self
    }
}

pub trait DataLockFactory<T> {
    type Lock: DataLock<Target = T>;
    /// Contruct a new lock from the given initial value.
    fn new(init: T) -> Self::Lock;
}

pub struct RevisedData<T> {
    pub data: T,
    rev: AtomicUsize,
}

impl<L> RevisedData<L>
where
    L: DataLock,
{
    pub fn new_from<LF: DataLockFactory<L::Target, Lock = L>>(init: L::Target) -> Self {
        Self {
            data: LF::new(init),
            rev: AtomicUsize::default(),
        }
    }

    pub fn spawn_observer(&self) -> RevisedDataObserver<L> {
        RevisedDataObserver {
            inner: self,
            rev: self.rev.load(Ordering::SeqCst),
        }
    }
}

pub struct RevisedDataObserver<'a, L> {
    inner: &'a RevisedData<L>,
    rev: usize,
}

impl<'a, L> RevisedDataObserver<'a, L>
where
    Self: SensorOut,
{
    pub fn fuse<B, T, F>(self, other: B, f: F) -> RevisedDataObserverFused<Self, B, T, F>
    where
        B: SensorOut + 'a,
        F: 'a
            + FnMut(
                &<<Self as SensorOut>::Lock as DataReadLock>::Target,
                &<<B as SensorOut>::Lock as DataReadLock>::Target,
            ) -> T,
    {
        RevisedDataObserverFused::fuse_with(self, other, f)
    }
}

impl<T> RevisedData<T> {
    pub fn update_rev(&self) {
        self.rev.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn rev(&self) -> usize {
        self.rev.load(core::sync::atomic::Ordering::SeqCst)
    }
}

trait SensorIn {
    type Lock: DataLock;
    fn write(&self) -> <Self::Lock as DataLock>::WriteGuard<'_>;
    fn read(&self) -> <Self::Lock as DataReadLock>::ReadGuard<'_>;
    #[inline]
    fn update(&self, sample: <Self::Lock as DataReadLock>::Target) {
        let mut guard = self.write();
        *guard = sample;
        self.mark_all_unseen();
        drop(guard);
    }
    #[inline(always)]
    fn modify_with(&self, f: impl FnOnce(&mut <Self::Lock as DataReadLock>::Target)) {
        let mut guard = self.write();
        f(&mut guard);
        self.mark_all_unseen();
        drop(guard);
    }
    fn mark_all_unseen(&self);
}

pub trait SensorOut {
    type Lock: DataReadLock;

    /// Returns the latest value obtainable by the sensor. The sesnor's internal cache is guaranteed to
    /// be updated after this call if the sensor is cached.
    #[inline(always)]
    fn pull_updated(&mut self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        self.mark_seen();
        self.pull()
    }
    /// Returns the current cached value of the sensor. This is guaranteed to be the latest value if the sensor
    /// is not cached.
    fn pull(&self) -> <Self::Lock as DataReadLock>::ReadGuard<'_>;
    /// Mark the current sensor data as seen.
    fn mark_seen(&mut self);
    /// Mark the current sensor data as unseen.
    fn mark_unseen(&mut self);
    fn has_changed(&self) -> bool;
    /// Returns true if `pull` may produce stale results.
    fn cached(&self) -> bool;
}

impl<T, L> SensorIn for RevisedData<L>
where
    L: DataLock<Target = T>,
{
    type Lock = L;

    #[inline(always)]
    fn write(&self) -> L::WriteGuard<'_> {
        self.data.write()
    }

    #[inline(always)]
    fn read(&self) -> L::ReadGuard<'_> {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<T, L, S> SensorIn for S
where
    L: DataLock<Target = T>,
    S: Deref<Target = RevisedData<L>> + ?Sized,
{
    type Lock = L;
    #[inline(always)]
    fn write(&self) -> L::WriteGuard<'_> {
        self.data.write()
    }

    #[inline(always)]
    fn read(&self) -> L::ReadGuard<'_> {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<'a, T, L> SensorOut for RevisedDataObserver<'a, L>
where
    L: DataReadLock<Target = T>,
{
    type Lock = L;

    #[inline(always)]
    fn pull(&self) -> L::ReadGuard<'_> {
        self.inner.data.read()
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.rev = self.inner.rev.load(Ordering::SeqCst);
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.rev = self.inner.rev.load(Ordering::SeqCst).wrapping_sub(1);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.rev == self.inner.rev.load(Ordering::SeqCst)
    }
    #[inline(always)]
    fn cached(&self) -> bool {
        false
    }
}

pub struct RevisedDataObserverFused<A, B, T, F> {
    a: A,
    b: B,
    cached: FalseReadLock<T>,
    compute_func: F,
}

impl<A, B, T, F> RevisedDataObserverFused<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(&<A::Lock as DataReadLock>::Target, &<B::Lock as DataReadLock>::Target) -> T,
{
    #[inline(always)]
    pub fn fuse_with(a: A, b: B, mut f: F) -> Self {
        let cached = FalseReadLock(f(&a.pull(), &b.pull()));
        Self {
            a,
            b,
            cached,
            compute_func: f,
        }
    }
}

impl<A, B, T, F> SensorOut for RevisedDataObserverFused<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(&<A::Lock as DataReadLock>::Target, &<B::Lock as DataReadLock>::Target) -> T,
{
    type Lock = FalseReadLock<T>;

    #[inline]
    fn pull_updated(&mut self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        *self.cached = (self.compute_func)(&self.a.pull(), &self.b.pull());
        &self.cached
    }

    #[inline(always)]
    fn pull(&self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        &self.cached
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.a.mark_seen();
        self.b.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.a.mark_unseen();
        self.b.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.a.has_changed() || self.b.has_changed()
    }

    #[inline(always)]
    fn cached(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{parking_lot::RwSensor, SensorIn, SensorOut};

    #[test]
    fn test_1() {
        let s1 = RwSensor::new(-3);
        let s2 = RwSensor::new(5);

        let mut x = s1
            .spawn_observer()
            .fuse(s2.spawn_observer(), |x, y| *x + *y);

        assert_eq!(*x.pull(), 2);
        *s1.write() = 3;

        assert_eq!(*x.pull_updated(), 8);
    }
}
