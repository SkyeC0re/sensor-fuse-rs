use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};
use parking_lot;

pub trait RwLock {
    type Target;
    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self::Target: 'read,
        Self: 'read;
    type WriteGuard<'write>: Deref<Target = Self::Target> + DerefMut
    where
        Self::Target: 'write,
        Self: 'write;

    /// Contruct a new lock from the given initial value.
    fn new(init: Self::Target) -> Self;
    /// Immutable access to the current value inside the lock.
    fn read(&self) -> Self::ReadGuard<'_>;
    /// Mutable access to the current value inside the lock.
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

impl<T> RwLock for parking_lot::RwLock<T> {
    type Target = T;
    type ReadGuard<'me> = parking_lot::RwLockReadGuard<'me, T> where T: 'me;
    type WriteGuard<'me> = parking_lot::RwLockWriteGuard<'me, T> where T: 'me;

    #[inline(always)]
    fn new(init: T) -> Self {
        Self::new(init)
    }

    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read()
    }

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write()
    }

    #[inline(always)]
    fn downgrade<'a>(&'a self, write_guard: Self::WriteGuard<'a>) -> Self::ReadGuard<'a> {
        parking_lot::RwLockWriteGuard::downgrade(write_guard)
    }
}

impl<T> RwLock for std::sync::RwLock<T> {
    type Target = T;

    type ReadGuard<'read> = std::sync::RwLockReadGuard<'read, T> where T: 'read;

    type WriteGuard<'write> = std::sync::RwLockWriteGuard<'write, T> where T: 'write;

    fn new(init: Self::Target) -> Self {
        Self::new(init)
    }

    fn read(&self) -> Self::ReadGuard<'_> {
        self.read().unwrap()
    }

    fn write(&self) -> Self::WriteGuard<'_> {
        self.write().unwrap()
    }
}

pub struct SyncronizedData<L> {
    pub data: L,
    rev: AtomicUsize,
}

impl<L> SyncronizedData<L>
where
    L: RwLock,
{
    pub fn new(init: L::Target) -> Self {
        Self {
            data: RwLock::new(init),
            rev: AtomicUsize::default(),
        }
    }

    pub fn spawn_observer(&self) -> SyncronizedDataObserver<L> {
        SyncronizedDataObserver {
            inner: self,
            rev: self.rev.load(Ordering::SeqCst),
        }
    }
}

pub struct SyncronizedDataObserver<'a, L> {
    inner: &'a SyncronizedData<L>,
    rev: usize,
}

impl<T> SyncronizedData<T> {
    pub fn update_rev(&self) {
        self.rev.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn rev(&self) -> usize {
        self.rev.load(core::sync::atomic::Ordering::SeqCst)
    }
}

trait SensorWrite {
    type Lock: RwLock;
    fn borrow_mut(&self) -> <Self::Lock as RwLock>::WriteGuard<'_>;
    fn borrow(&self) -> <Self::Lock as RwLock>::ReadGuard<'_>;
    #[inline]
    fn update(&self, sample: <Self::Lock as RwLock>::Target) {
        let mut guard = self.borrow_mut();
        *guard = sample;
        self.mark_all_unseen();
        drop(guard);
    }
    #[inline(always)]
    fn modify_with(&self, f: impl FnOnce(&mut <Self::Lock as RwLock>::Target)) {
        let mut guard = self.borrow_mut();
        f(&mut guard);
        self.mark_all_unseen();
        drop(guard);
    }
    fn mark_all_unseen(&self);
}

pub trait SensorRead {
    type Lock: RwLock;

    #[inline(always)]
    fn borrow_and_update(&mut self) -> <Self::Lock as RwLock>::ReadGuard<'_> {
        self.mark_seen();
        self.borrow()
    }
    fn borrow(&self) -> <Self::Lock as RwLock>::ReadGuard<'_>;
    fn mark_seen(&mut self);
    fn mark_unseen(&mut self);
    fn has_changed(&self) -> bool;
}

impl<T, L> SensorWrite for SyncronizedData<L>
where
    L: RwLock<Target = T>,
{
    type Lock = L;

    #[inline(always)]
    fn borrow_mut(&self) -> L::WriteGuard<'_> {
        self.data.write()
    }

    #[inline(always)]
    fn borrow(&self) -> L::ReadGuard<'_> {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<'a, T, L, S> SensorWrite for S
where
    L: RwLock<Target = T>,
    S: Deref<Target = SyncronizedData<L>> + ?Sized,
{
    type Lock = L;
    #[inline(always)]
    fn borrow_mut(&self) -> L::WriteGuard<'_> {
        self.data.write()
    }

    #[inline(always)]
    fn borrow(&self) -> L::ReadGuard<'_> {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<'a, T, L> SensorRead for SyncronizedDataObserver<'a, L>
where
    L: RwLock<Target = T>,
{
    type Lock = L;
    fn borrow(&self) -> L::ReadGuard<'_> {
        self.inner.data.read()
    }

    #[inline]
    fn mark_seen(&mut self) {
        self.rev = self.inner.rev.load(Ordering::SeqCst);
    }

    #[inline]
    fn mark_unseen(&mut self) {
        self.rev = self.inner.rev.load(Ordering::SeqCst).wrapping_sub(1);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.rev == self.inner.rev.load(Ordering::SeqCst)
    }
}

pub struct FusedSensorObserver<T1, T2, L, F> {
    t1: T1,
    t2: T2,
    lock: L,
    compute_func: F,
}

impl<'a, T1, T2, L, F> FusedSensorObserver<T1, T2, L, F>
where
    T1: SensorRead,
    T2: SensorRead,
    L: RwLock,
    F: FnMut(
        &<<T1 as SensorRead>::Lock as RwLock>::Target,
        &<<T2 as SensorRead>::Lock as RwLock>::Target,
    ) -> <L as RwLock>::Target,
{
    #[inline(always)]
    pub fn new(a: T1, b: T2, mut f: F) -> Self {
        let lock = L::new(f(&a.borrow(), &b.borrow()));
        Self {
            t1: a,
            t2: b,
            lock,
            compute_func: f,
        }
    }
}
pub type RwSensor<T> = SyncronizedData<parking_lot::RwLock<T>>;

impl<T1, T2, L, F> SensorRead for FusedSensorObserver<T1, T2, L, F>
where
    T1: SensorRead,
    T2: SensorRead,
    L: RwLock,
    F: FnMut(&<T1::Lock as RwLock>::Target, &<T2::Lock as RwLock>::Target) -> L::Target,
{
    type Lock = L;

    #[inline]
    fn borrow_and_update<'c>(&'c mut self) -> L::ReadGuard<'_> {
        let new_val = (self.compute_func)(&self.t1.borrow(), &self.t2.borrow());
        let mut guard = self.lock.write();
        *guard = new_val;
        self.lock.downgrade(guard)
    }

    #[inline(always)]
    fn borrow(&self) -> L::ReadGuard<'_> {
        self.lock.read()
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.t1.mark_seen();
        self.t2.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.t1.mark_unseen();
        self.t2.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.t1.has_changed() || self.t2.has_changed()
    }
}

#[cfg(test)]
mod tests {
    use crate::{FusedSensorObserver, RwSensor, SensorRead};

    #[test]
    fn test_1() {
        let s1 = RwSensor::new(-3);
        let s2 = RwSensor::new(5);

        let x = FusedSensorObserver::<_, _, parking_lot::RwLock<_>, _>::new(
            s1.spawn_observer(),
            s2.spawn_observer(),
            |x, y| *x + *y,
        );

        assert_eq!(*x.borrow(), 2);
    }
}
