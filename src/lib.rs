use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};
use parking_lot;

pub trait RwLock<'a> {
    type Target;
    type ReadGuard: Deref<Target = Self::Target>;
    type WriteGuard: Deref<Target = Self::Target> + DerefMut;

    /// Contruct a new lock from the given initial value.
    fn new(init: Self::Target) -> Self;
    /// Immutable access to the current value inside the lock.
    fn read(&'a self) -> Self::ReadGuard;
    /// Mutable access to the current value inside the lock.
    fn write(&'a self) -> Self::WriteGuard;

    /// Downgrade mutable access to immutable access for a write guard originating from this lock instance.
    ///
    /// # Panics
    ///
    /// Behaviour is only well defined if and only if `write_guard` originates from self.
    /// May panic if `wirte_guard` does not originate from `self`. Implementations may also downgrade the guard
    /// regardless or return a new read guard originating from `self`.
    #[inline(always)]
    fn downgrade(&'a self, write_guard: Self::WriteGuard) -> Self::ReadGuard {
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
    fn upgrade(&'a self, read_guard: Self::ReadGuard) -> Self::WriteGuard {
        drop(read_guard);
        self.write()
    }
}

impl<'a, T> RwLock<'a> for parking_lot::RwLock<T>
where
    T: 'a,
{
    type Target = T;
    type ReadGuard = parking_lot::RwLockReadGuard<'a, T>;
    type WriteGuard = parking_lot::RwLockWriteGuard<'a, T>;

    #[inline(always)]
    fn new(init: T) -> Self {
        Self::new(init)
    }

    #[inline(always)]
    fn read(&'a self) -> Self::ReadGuard {
        self.read()
    }

    #[inline(always)]
    fn write(&'a self) -> Self::WriteGuard {
        self.write()
    }

    #[inline(always)]
    fn downgrade(&self, write_guard: Self::WriteGuard) -> Self::ReadGuard {
        parking_lot::RwLockWriteGuard::downgrade(write_guard)
    }
}

impl<'a, T> RwLock<'a> for std::sync::RwLock<T>
where
    T: 'a,
{
    type Target = T;

    type ReadGuard = std::sync::RwLockReadGuard<'a, T>;

    type WriteGuard = std::sync::RwLockWriteGuard<'a, T>;

    fn new(init: Self::Target) -> Self {
        Self::new(init)
    }

    fn read(&'a self) -> Self::ReadGuard {
        self.read().unwrap()
    }

    fn write(&'a self) -> Self::WriteGuard {
        self.write().unwrap()
    }
}

pub struct SyncronizedData<T> {
    pub data: T,
    rev: AtomicUsize,
}

pub struct SyncronizedDataObserver<'a, T> {
    inner: &'a SyncronizedData<T>,
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

trait SensorWrite<'a> {
    type Lock: RwLock<'a>;
    fn borrow_mut(&'a self) -> <Self::Lock as RwLock<'a>>::WriteGuard;
    fn borrow(&'a self) -> <Self::Lock as RwLock<'a>>::ReadGuard;
    #[inline]
    fn update(&'a self, sample: <Self::Lock as RwLock<'a>>::Target) {
        let mut guard = self.borrow_mut();
        *guard = sample;
        self.mark_all_unseen();
        drop(guard);
    }
    #[inline(always)]
    fn modify_with(&'a self, f: impl FnOnce(&mut <Self::Lock as RwLock<'a>>::Target)) {
        let mut guard = self.borrow_mut();
        f(&mut guard);
        self.mark_all_unseen();
        drop(guard);
    }
    fn mark_all_unseen(&self);
}

trait SensorRead<'a> {
    type Lock: RwLock<'a>;

    #[inline(always)]
    fn borrow_and_update(&'a mut self) -> <Self::Lock as RwLock<'a>>::ReadGuard {
        self.mark_seen();
        self.borrow()
    }
    fn borrow(&'a self) -> <Self::Lock as RwLock<'a>>::ReadGuard;
    fn mark_seen(&mut self);
    fn mark_unseen(&mut self);
    fn has_changed(&self) -> bool;
}

impl<'a, T, L> SensorWrite<'a> for SyncronizedData<L>
where
    L: RwLock<'a, Target = T> + 'a,
{
    type Lock = L;

    #[inline(always)]
    fn borrow_mut(&'a self) -> L::WriteGuard {
        self.data.write()
    }

    #[inline(always)]
    fn borrow(&'a self) -> L::ReadGuard {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<'a, T, L, S> SensorWrite<'a> for S
where
    L: RwLock<'a, Target = T> + 'a,
    S: Deref<Target = SyncronizedData<L>> + ?Sized,
{
    type Lock = L;
    #[inline(always)]
    fn borrow_mut(&'a self) -> L::WriteGuard {
        self.data.write()
    }

    #[inline(always)]
    fn borrow(&'a self) -> L::ReadGuard {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<'a, T, L> SensorRead<'a> for SyncronizedDataObserver<'a, L>
where
    L: RwLock<'a, Target = T> + 'a,
{
    type Lock = L;
    fn borrow(&'a self) -> L::ReadGuard {
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

impl<'a, T1, O1, L1, T2, O2, L2, O, L, F> SensorRead<'a> for FusedSensorObserver<T1, T2, L, F>
where
    T1: SensorRead<'a, Lock = L1>,
    L1: RwLock<'a, Target = O1>,
    T2: SensorRead<'a, Lock = L2>,
    L2: RwLock<'a, Target = O2>,
    L: RwLock<'a, Target = O>,
    F: FnMut(&O1, &O2) -> O,
{
    type Lock = L;

    #[inline]
    fn borrow_and_update(&'a mut self) -> L::ReadGuard {
        let new_val = (self.compute_func)(&self.t1.borrow(), &self.t2.borrow());
        let mut guard = self.lock.write();
        *guard = new_val;
        self.lock.downgrade(guard)
    }

    #[inline(always)]
    fn borrow(&'a self) -> <Self::Lock as RwLock<'a>>::ReadGuard {
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
mod tests {}
