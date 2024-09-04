//! # Sensor-Fuse
//! An unopinionated no-std compatible observer/callback framework for the opinionated user.
//!
//! Designed as generalization to [Tokio's watch channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html), Sensor-Fuse aims
//! to provide a modular framework for implementing observer or observer + callback patterns. From the perspective of this crate sensor data can
//! be defined as the following:
//! - The raw data.
//! - A callback executor which does some work whenever the raw data is updated.
//! - An atomic version counter.
//! - A locking strategy that allows atomic access to the raw data and callback executor.
//! - An optional heap pointer that wraps all of the above.
//!
//! Given this definition, Tokio's watch channel only allows for the generalization of the raw data type, however
//! Sensor-Fuse additionally allows for the generalization of the callback executor, the locking strategy and optional heap pointer as well.
//! In fact, Tokio's watch channel within this framework is roughly equivalent to selecting `std::sync::RwLock` as the locking strategy,
//! and the standard `VecBox` executor (the core requirement here being that the executor must be able to accept `Waker`s) with
//! an `Arc` pointer wrapping everything.
//!
//! ```
//! extern crate sensor_fuse;
//! use sensor_fuse::{prelude::*, lock::std_sync::RwSensorWriterExec};
//!
//! let writer = RwSensorWriterExec::new(3);
//! writer.register(|x: &i32 | {
//!     println!("{}", x);
//!     // The standard executor uses a boolean return value to determine whether or not to keep the
//!     // function in its execution set. To keep the function forever, we just unconditionally return true.
//!     true
//! });
//!
//! let mut observer = writer.spawn_observer();
//! assert_eq!(*observer.pull(), 3);
//!
//! // Prints "5".
//! writer.update(5);
//!
//! assert_eq!(*observer.pull(), 5);
//! ```
//!
//! ## Execution Optimization
//!
//! The library is opinionated in one aspect. It assumes that the majority of accesses to the executor originates from sensor
//! value updates as opposed to executable registrations and as such can be bundled under the same locking mechanism as the sensor's
//! raw data. This allows the executor to be mutably borrowed for each sensor value update, removing the need to lock the executor
//! separately on every sensor value update. The cost, however associated with this is that registering an executable

#![warn(bad_style)]
#![warn(missing_docs)]
#![warn(trivial_casts)]
#![warn(trivial_numeric_casts)]
#![warn(unused)]
#![warn(unused_extern_crates)]
#![warn(unused_import_braces)]
#![warn(unused_qualifications)]
#![warn(unused_results)]

pub mod callback;
pub mod lock;
pub mod prelude;

use core::{
    future::Future,
    marker::PhantomData,
    ops::Deref,
    ops::DerefMut,
    ptr::null_mut,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};
#[cfg(feature = "std")]
use std::sync::Arc;

use callback::{
    AccessStrategyImmut, AccessStrategyMut, ExecManagerMut, ExecRegisterMut, ExecutionStrategy,
    RegistrationStrategy,
};

use crate::{
    callback::{ExecManager, ExecRegister},
    lock::{
        DataReadLock, DataWriteLock, FalseReadLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier,
    },
};

/*** Revised Data ***/

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If it's not broken don't fix it.
const CLOSED_BIT: usize = 1;
const STEP_SIZE: usize = 2;

pub struct RevisedData<T> {
    pub data: T,
    version: AtomicUsize,
}

impl<T> Deref for RevisedData<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> DerefMut for RevisedData<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl<T> RevisedData<T> {
    #[inline(always)]
    pub const fn new(data: T) -> Self {
        Self {
            data,
            version: AtomicUsize::new(0),
        }
    }

    #[inline(always)]
    pub fn update_version(&self, step_size: usize) {
        self.version
            .fetch_add(step_size, core::sync::atomic::Ordering::Release);
    }

    #[inline(always)]
    pub fn version(&self) -> usize {
        self.version.load(core::sync::atomic::Ordering::Acquire)
    }
}

/// Trait for sharing a (most likely heap pointer) wrapped `struct@RevisedData` with a locking strategy.
pub trait ShareStrategy<'a> {
    type Target;
    type Shared: Deref<Target = RevisedData<Self::Target>>;

    /// Share the revised data for the largest possible lifetime.
    fn share_lock(self) -> Self::Shared;

    /// There is probably a better way than this to get an elided reference
    /// to the revised data.
    fn share_elided_ref(self) -> &'a RevisedData<Self::Target>;
}

impl<'a, T> ShareStrategy<'a> for &'a RevisedData<T> {
    type Target = T;
    type Shared = Self;
    #[inline(always)]
    fn share_lock(self) -> Self {
        self
    }

    #[inline(always)]
    fn share_elided_ref(self) -> Self {
        self
    }
}

#[cfg(feature = "std")]
impl<'a, T> ShareStrategy<'a> for &'a Arc<RevisedData<T>> {
    type Target = T;
    type Shared = Arc<RevisedData<T>>;

    #[inline(always)]
    fn share_lock(self) -> Self::Shared {
        self.clone()
    }

    #[inline(always)]
    fn share_elided_ref(self) -> &'a RevisedData<T> {
        self
    }
}

/*** Sensor Writing ***/

#[repr(transparent)]
pub struct SensorWriter<T, S, L, E = ()>(S)
where
    L: DataWriteLock<Target = T>,
    E: ExecutionStrategy<T>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>;

impl<T, S, L, E> Drop for SensorWriter<T, S, L, E>
where
    L: DataWriteLock<Target = T>,
    E: ExecutionStrategy<T>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
{
    fn drop(&mut self) {
        let x = &self.0;
        x.share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

impl<T, S, L, E> SensorWriter<T, S, L, E>
where
    L: DataWriteLock<Target = T>,
    E: ExecutionStrategy<T>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
{
    /// Acquire a read lock on the underlying data.
    #[inline(always)]
    pub fn read(&self) -> <L as ReadGuardSpecifier>::ReadGuard<'_> {
        self.0.share_elided_ref().data.0.read()
    }

    /// Acquire a write lock on the underlying data.
    #[inline(always)]
    pub fn write(&self) -> <L as DataWriteLock>::WriteGuard<'_> {
        self.0.share_elided_ref().data.0.write()
    }

    /// Update the sensor value, notify observers and execute all registered callbacks.
    #[inline]
    pub fn update(&self, sample: T) {
        let revised_data = self.0.share_elided_ref();
        let mut guard = revised_data.data.0.write();
        *guard = sample;
        revised_data.update_version(STEP_SIZE);
        let guard = L::atomic_downgrade(guard);

        // Atomic downgrade just occured. No other modication can happen.
        revised_data.1.execute(&guard);
    }

    /// Modify the sensor value in place, notify observers and execute all registered callbacks.
    #[inline]
    pub fn modify_with(&self, f: impl FnOnce(&mut T)) {
        let revised_data = self.0.share_elided_ref();
        let mut guard = revised_data.data.0.write();
        f(&mut guard);
        revised_data.update_version(STEP_SIZE);
        let guard = L::atomic_downgrade(guard);

        // Atomic downgrade just occured. No other modification can happen.
        revised_data.1.execute(&guard);
    }

    /// Mark the current sensor value as unseen to all observers, notify them and execute all registered callbacks.
    #[inline]
    pub fn mark_all_unseen(&self) {
        let revised_data = self.0.share_elided_ref();
        let guard = L::atomic_downgrade(revised_data.data.0.write());
        revised_data.update_version(STEP_SIZE);

        // Atomic downgrade just occured. No other modification can happen.
        revised_data.1.execute(&guard);
    }

    /// Spawn an observer by immutably borrowing the sensor writer's data. By definition this observer's scope will be limited
    /// by the scope of the writer.
    #[inline(always)]
    pub fn spawn_referenced_observer(&self) -> SensorObserver<T, &'_ RevisedData<(L, E)>, L, E> {
        let inner = self.0.share_elided_ref();
        SensorObserver {
            inner,
            version: inner.version(),
            // _types: PhantomData,
        }
    }

    /// Spawn an observer by leveraging the sensor writer's sharing strategy. May allow the observer
    /// to outlive the sensor writer for an appropriate sharing strategy (such as if the writer wraps its data in an `Arc`).
    #[inline(always)]
    pub fn spawn_observer(&self) -> SensorObserver<T, <&'_ S as ShareStrategy>::Shared, L, E> {
        let inner = self.0.share_lock();
        SensorObserver {
            version: inner.version(),
            inner,
            // _types: PhantomData,
        }
    }
}

/*** Sensor Observation ***/

pub trait SensorObserve {
    type Lock: ReadGuardSpecifier;

    /// Returns the latest value obtainable by the sensor. The sensor's internal cache is guaranteed to
    /// be updated after this call if the sensor is cached. After a call to this function, the obtained
    /// sensor value will be marked as seen.
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Returns the current cached value of the sensor. This is guaranteed to be the latest value if the sensor
    /// is not cached. A call to this function will however **not** mark the value as seen, even if the value is not cached.
    fn borrow(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Mark the current sensor data as seen.
    fn mark_seen(&mut self);
    /// Mark the current sensor data as unseen.
    fn mark_unseen(&mut self);
    /// Returns true if the sensor data has been marked as unseen.
    fn has_changed(&self) -> bool;
    /// Returns true if `borrow` may produce stale results.
    fn is_cached(&self) -> bool;
    /// Returns true if all upstream writers has been dropped and no more updates can occur.
    fn is_closed(&self) -> bool;

    #[inline(always)]
    fn fuse<B, T, F>(self, other: B, f: F) -> FusedSensorObserver<Self, B, T, F>
    where
        Self: Sized,
        B: SensorObserve,
        F: FnMut(
            &<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
            &<<B as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
        ) -> T,
    {
        FusedSensorObserver::fuse_with(self, other, f)
    }

    #[inline(always)]
    fn fuse_cached<B, T, F>(self, other: B, f: F) -> FusedSensorObserverCached<Self, B, T, F>
    where
        Self: Sized,
        B: SensorObserve,
        F: FnMut(
            &<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
            &<<B as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
        ) -> T,
    {
        FusedSensorObserverCached::fuse_with(self, other, f)
    }

    #[inline(always)]
    fn map<T, F>(self, f: F) -> MappedSensorObserver<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserver::map_with(self, f)
    }

    #[inline(always)]
    fn map_cached<T, F>(self, f: F) -> MappedSensorObserverCached<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserverCached::map_with(self, f)
    }
}

pub struct SensorObserver<T, R, L, E = L>
where
    E: ExecutionStrategy<T>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
{
    inner: R,
    version: usize,
    // _types: PhantomData<(L, E)>,
}

impl<T, R, L, E> Clone for SensorObserver<T, R, L, E>
where
    E: ExecutionStrategy<T>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            version: self.version,
            // _types: PhantomData,
        }
    }
}

impl<T, R, L, E> SensorObserve for SensorObserver<T, R, L, E>
where
    E: ExecutionStrategy<T>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
{
    type Lock = L;

    #[inline(always)]
    fn borrow(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.inner.data.0.read()
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.mark_seen();
        self.inner.data.0.read()
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.version = self.inner.version();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.version = self.inner.version().wrapping_sub(STEP_SIZE);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.version >> 1 != self.inner.version() >> 1
    }
    #[inline(always)]
    fn is_cached(&self) -> bool {
        false
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.version() & CLOSED_BIT == CLOSED_BIT
    }
}

/*** Mapped and Fused Observers ***/

pub struct MappedSensorObserver<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub inner: A,
    _false_cache: OwnedFalseLock<T>,
    map: F,
}

impl<A, T, F> MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    #[inline(always)]
    pub fn map_with(a: A, f: F) -> Self {
        Self {
            inner: a,
            _false_cache: OwnedFalseLock::new(),
            map: f,
        }
    }
}

impl<A, T, F> SensorObserve for MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    type Lock = OwnedFalseLock<T>;

    #[inline]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.map)(&self.inner.pull()))
    }

    #[inline(always)]
    fn borrow(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.map)(&self.inner.borrow()))
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.inner.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.inner.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.has_changed()
    }

    #[inline(always)]
    fn is_cached(&self) -> bool {
        false
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

pub struct MappedSensorObserverCached<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub inner: A,
    cached: FalseReadLock<T>,
    map: F,
}

impl<A, T, F> MappedSensorObserverCached<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    #[inline(always)]
    pub fn map_with(mut a: A, mut f: F) -> Self {
        let cached = FalseReadLock(f(&mut a.borrow()));
        Self {
            inner: a,
            cached,
            map: f,
        }
    }
}

impl<A, T, F> SensorObserve for MappedSensorObserverCached<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    type Lock = FalseReadLock<T>;

    #[inline]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        *self.cached = (self.map)(&self.inner.pull());
        &self.cached
    }

    #[inline(always)]
    fn borrow(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        &self.cached
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.inner.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.inner.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.has_changed()
    }

    #[inline(always)]
    fn is_cached(&self) -> bool {
        true
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}
pub struct FusedSensorObserver<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub a: A,
    pub b: B,
    _false_cache: OwnedFalseLock<T>,
    fuse: F,
}

impl<A, B, T, F> FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    #[inline(always)]
    pub fn fuse_with(a: A, b: B, f: F) -> Self {
        Self {
            a,
            b,
            _false_cache: OwnedFalseLock::new(),
            fuse: f,
        }
    }
}

impl<A, B, T, F> SensorObserve for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    type Lock = OwnedFalseLock<T>;

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.fuse)(&self.a.pull(), &self.b.pull()))
    }

    #[inline(always)]
    fn borrow(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.fuse)(&self.a.borrow(), &self.b.borrow()))
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
    fn is_cached(&self) -> bool {
        self.a.is_cached() || self.b.is_cached()
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }
}

pub struct FusedSensorObserverCached<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub a: A,
    pub b: B,
    cache: FalseReadLock<T>,
    fuse: F,
}

impl<A, B, T, F> FusedSensorObserverCached<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    #[inline(always)]
    pub fn fuse_with(mut a: A, mut b: B, mut f: F) -> Self {
        let cache = FalseReadLock(f(&a.borrow(), &b.borrow()));
        Self {
            a,
            b,
            cache,
            fuse: f,
        }
    }
}

impl<A, B, T, F> SensorObserve for FusedSensorObserverCached<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    type Lock = FalseReadLock<T>;

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        *self.cache = (self.fuse)(&self.a.pull(), &self.b.pull());
        &self.cache
    }

    #[inline(always)]
    fn borrow(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        &self.cache
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
    fn is_cached(&self) -> bool {
        true
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }
}

/*** Callbacks and Async ***/

pub trait RegisterFunction<F> {
    fn register(&self, f: F);
}

impl<T, S, L, E, F> RegisterFunction<F> for SensorWriter<T, S, L, AccessStrategyMut<T, E>>
where
    L: DataWriteLock<Target = T>,
    E: ExecManagerMut<T> + ExecRegisterMut<F>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, AccessStrategyMut<T, E>)>,
{
    fn register(&self, f: F) {
        let revised_data = self.0.share_elided_ref();
        let guard = revised_data.data.0.write();
        let guard = L::atomic_downgrade(guard);
        revised_data.1.register(f);
        drop(guard);
    }
}

impl<T, S, L, E, F> RegisterFunction<F> for SensorWriter<T, S, L, AccessStrategyImmut<T, E>>
where
    L: DataWriteLock<Target = T>,
    E: ExecManager<T> + ExecRegister<F>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, AccessStrategyImmut<T, E>)>,
{
    fn register(&self, f: F) {
        self.0.share_elided_ref().1.register(f);
    }
}

impl<T, R, L, E, F> RegisterFunction<F> for SensorObserver<T, R, L, AccessStrategyMut<T, E>>
where
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, AccessStrategyMut<T, E>)>>,
    E: ExecManagerMut<T> + ExecRegisterMut<F>,
{
    fn register(&self, f: F) {
        let revised_data = self.inner.share_elided_ref();
        let guard = revised_data.data.0.write();
        let guard = L::atomic_downgrade(guard);
        revised_data.1.register(f);
        drop(guard);
    }
}

impl<T, R, L, E, F> RegisterFunction<F> for SensorObserver<T, R, L, AccessStrategyImmut<T, E>>
where
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, AccessStrategyImmut<T, E>)>>,
    E: ExecManager<T> + ExecRegister<F>,
{
    fn register(&self, f: F) {
        self.inner.share_elided_ref().1.register(f);
    }
}

#[repr(transparent)]
pub struct WaitUntilChangedFuture<'a, T, R, L, E>(Option<&'a SensorObserver<T, R, L, E>>)
where
    E: ExecutionStrategy<T>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>;

impl<'a, T, R, L, E> Future for WaitUntilChangedFuture<'a, T, R, L, E>
where
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
    E: ExecutionStrategy<T>,
    for<'b> SensorObserver<T, R, L, E>: RegisterFunction<&'b Waker>,
    Self: Unpin,
{
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        if let Some(value) = self.0.take() {
            if value.has_changed() {
                Poll::Ready(())
            } else {
                value.register(cx.waker());
                self.0 = Some(value);
                Poll::Pending
            }
        } else {
            #[cfg(debug_assertions)]
            panic!("Poll called after future returned ready.");
            #[cfg(not(debug_assertions))]
            Poll::Pending
        }
    }
}

pub struct WaitForFuture<'a, T, R, L, E, F>(
    *mut SensorObserver<T, R, L, E>,
    F,
    PhantomData<&'a ()>,
)
where
    E: ExecutionStrategy<T>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>;

impl<'a, T: 'a, R: 'a, L: 'a, E: 'a, F> Future for WaitForFuture<'a, T, R, L, E, F>
where
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
    E: ExecutionStrategy<T>,
    for<'b> SensorObserver<T, R, L, E>: RegisterFunction<&'b Waker>,
    Self: Unpin,
    F: Send + FnMut(&T) -> bool,
{
    type Output = L::ReadGuard<'a>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        // Safe code does not seem to want to compile without a guard reaquisition. Possibly inexperience,
        // but also possibly a quirk of `Option`'s `None` variant still storing type information.
        if !self.0.is_null() {
            let mut observer = unsafe { &mut *self.0 };
            let changed = observer.has_changed();
            let guard = observer.pull();
            if changed && (self.1)(&guard) {
                self.0 = null_mut();
                return Poll::Ready(guard);
            } else {
                drop(guard);
                observer = unsafe { &mut *self.0 };
                observer.register(cx.waker());
                return Poll::Pending;
            }
        }

        #[cfg(debug_assertions)]
        panic!("Poll called after future returned ready.");
        #[cfg(not(debug_assertions))]
        Poll::Pending
    }
}

impl<T, R, L, E> SensorObserver<T, R, L, E>
where
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
    E: ExecutionStrategy<T>,
    for<'b> SensorObserver<T, R, L, E>: RegisterFunction<&'b Waker>,
{
    /// Asyncronously wait until the sensor value is updated. This call will **not** update the observer's version,
    /// as such an additional call to `pull` or `pull_updated` is required.
    pub fn wait_until_changed(&self) -> WaitUntilChangedFuture<'_, T, R, L, E> {
        WaitUntilChangedFuture(Some(self))
    }
    /// Asyncronously wait until the sensor value has been updated with a value that satisfies a condition. This call **will** update the observer's version
    /// and evaluate the condition function on values obtained by `pull_updated`.
    pub fn wait_for<F: Send + FnMut(&T) -> bool>(
        &mut self,
        f: F,
    ) -> WaitForFuture<'_, T, R, L, E, F> {
        WaitForFuture(self, f, PhantomData)
    }
}
