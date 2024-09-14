//! # Sensor-Fuse
//! An un-opinionated no-std compatible observer/callback framework for the opinionated user.
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
//! use sensor_fuse::{prelude::*, lock::parking_lot::RwSensorWriterExec};
//!
//! let writer = RwSensorWriterExec::new(3);
//!
//! writer.register(Box::new(|x: &_| {
//!     println!("{}", x);
//!     // The standard executor uses a boolean return value to determine whether or not to keep the
//!     // function in its execution set. To keep the function forever, we just unconditionally return true.
//!     true
//! }));
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
//! ## Executors and Executables
//!
//! Within the framework, every sensor is associated with an executor which allows executables to be registered. In this context
//! an executable can be anything but fundamentally represents a piece of work that must occur each time the sensor value is updated. The frameworks basic
//! non-trivial (`()`) executor (`struct@StdExec`) for example allows for both the registration of `Waker`s to power asynchronous behaviour and
//! callback functions that act on references to the sensor value. Each time the sensor value is updated, `struct@StdExec` will execute and drop all `Waker`s and
//! all callbacks and drop those callbacks that produced `false` as output values.
//!

#![warn(bad_style)]
#![warn(missing_docs)]
#![warn(trivial_casts)]
#![warn(trivial_numeric_casts)]
#![warn(unused)]
#![warn(unused_extern_crates)]
#![warn(unused_import_braces)]
#![warn(unused_qualifications)]
#![warn(unused_results)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod executor;
pub mod lock;
pub mod prelude;

#[cfg(feature = "alloc")]
use alloc::sync::Arc;
use executor::{ExecManager, ExecRegister};

use core::{cell::UnsafeCell, pin::pin};
use core::{
    future::Future,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::null_mut,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use crate::lock::{DataReadLock, DataWriteLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier};

pub type SymResult<T> = Result<T, T>;

/*** Revised Data ***/

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If it's not broken don't fix it.
const CLOSED_BIT: usize = 1;
const STEP_SIZE: usize = 2;

/// Data structure for storing data with an atomic version tag.
pub struct RevisedData<T> {
    data: T,
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
    /// Initialize using the given initial data.
    #[inline(always)]
    const fn new(data: T) -> Self {
        Self {
            data,
            version: AtomicUsize::new(0),
        }
    }

    #[inline(always)]
    fn update_version(&self, step_size: usize) {
        let _ = self.version.fetch_add(step_size, Ordering::Release);
    }

    #[inline(always)]
    fn version(&self) -> usize {
        self.version.load(Ordering::Acquire)
    }
}

/// Trait for sharing a (most likely heap pointer) wrapped `struct@RevisedData` with a locking strategy.
pub trait ShareStrategy<'a> {
    /// The shared data.
    type Target;
    /// The wrapping container for the shared data.
    type Shared: Deref<Target = RevisedData<Self::Target>>;

    /// Share the revised data for the largest possible lifetime.
    fn share_data(self) -> Self::Shared;

    /// Create an immutable borrow to the underlying data, elided by the lifetime of
    /// wrapping container.
    fn share_elided_ref(self) -> &'a RevisedData<Self::Target>;
}

impl<'a, T> ShareStrategy<'a> for &'a RevisedData<T> {
    type Target = T;
    type Shared = Self;
    #[inline(always)]
    fn share_data(self) -> Self {
        self
    }

    #[inline(always)]
    fn share_elided_ref(self) -> Self {
        self
    }
}

#[cfg(feature = "alloc")]
impl<'a, T> ShareStrategy<'a> for &'a Arc<RevisedData<T>> {
    type Target = T;
    type Shared = Arc<RevisedData<T>>;

    #[inline(always)]
    fn share_data(self) -> Self::Shared {
        self.clone()
    }

    #[inline(always)]
    fn share_elided_ref(self) -> &'a RevisedData<T> {
        self
    }
}

/*** Sensor Writing ***/

/// The generalized sensor writer.
#[repr(transparent)]
pub struct SensorWriter<T, S, L, E = ()>(S)
where
    L: DataWriteLock<Target = T>,
    E: ExecManager<L>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>;

impl<T, S, L, E> SensorWriter<T, S, L, E>
where
    L: DataWriteLock<Target = T>,
    E: ExecManager<L>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
{
    /// Create a new sensor writer by wrapping the appropriate shared data.
    #[inline(always)]
    pub const fn new_from_shared(shared: S) -> Self {
        Self(shared)
    }
}

impl<T, S, L, E> Drop for SensorWriter<T, S, L, E>
where
    L: DataWriteLock<Target = T>,
    E: ExecManager<L>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
{
    fn drop(&mut self) {
        let x = &self.0;
        let _ = x
            .share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

impl<T, S, L, E> SensorWriter<T, S, L, E>
where
    L: DataWriteLock<Target = T>,
    E: ExecManager<L>,
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
        revised_data.1.execute(guard);
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
        revised_data.1.execute(guard);
    }

    /// Mark the current sensor value as unseen to all observers, notify them and execute all registered callbacks.
    #[inline]
    pub fn mark_all_unseen(&self) {
        let revised_data = self.0.share_elided_ref();
        let guard = L::atomic_downgrade(revised_data.data.0.write());
        revised_data.update_version(STEP_SIZE);

        // Atomic downgrade just occured. No other modification can happen.
        revised_data.1.execute(guard);
    }

    /// Spawn an observer by immutably borrowing the sensor writer's data. By definition this observer's scope will be limited
    /// by the scope of the writer.
    #[inline(always)]
    pub fn spawn_referenced_observer(&self) -> SensorObserver<T, &'_ RevisedData<(L, E)>, L, E> {
        let inner = self.0.share_elided_ref();
        SensorObserver {
            inner,
            version: UnsafeCell::new(inner.version()),
        }
    }

    /// Spawn an observer by leveraging the sensor writer's sharing strategy. May allow the observer
    /// to outlive the sensor writer for an appropriate sharing strategy (such as if the writer wraps its data in an `Arc`).
    #[inline(always)]
    pub fn spawn_observer(&self) -> SensorObserver<T, <&'_ S as ShareStrategy>::Shared, L, E> {
        let inner = self.0.share_data();
        SensorObserver {
            version: UnsafeCell::new(inner.version()),
            inner,
        }
    }
}

/*** Sensor Observation ***/

/// General observer functionality.
pub trait SensorObserve {
    /// The underlying locking mechanism associated with this observer.
    type Lock: ReadGuardSpecifier;

    /// Returns the latest value obtainable by the sensor and marks it as seen.
    fn pull(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Returns the latest value obtainable by the sensor without marking it as seen.
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Mark the current sensor data as seen.
    fn mark_seen(&self);
    /// Mark the current sensor data as unseen.
    fn mark_unseen(&self);
    /// Returns true if the sensor data has been marked as unseen.
    fn has_changed(&self) -> bool;
    /// Returns true if all upstream writers has been dropped and no more updates can occur.
    fn is_closed(&self) -> bool;
    /// Returns true if this observer has observed that all upstream writers has been dropped.
    /// This may return false even if all upstream writers have been dropped, but is guaranteed to
    /// be updated whenever a call to the following is made:
    /// - `function@pull`
    /// - `function@mark_seen`
    /// - `function@mark_unseen`
    /// - `functon@has_changed`
    /// - `function@is_closed`
    /// - `function@wait_until_changed` is awaited.
    /// - `function@wait_for` is awaited.
    fn is_locally_closed(&self) -> bool;

    /// Fuse this observer with another using the given fusion function into a cacheless observer.
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

    /// Map this observer into another using the given mapping function into a cacheless observer.
    #[inline(always)]
    fn map<T, F>(self, f: F) -> MappedSensorObserver<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserver::map_with(self, f)
    }
}

pub trait SensorObserveAsync: SensorObserve {
    /// Asyncronously wait until the sensor value is updated. This call will **not** update the observer's version,
    /// as such an additional call to `pull` or `pull_updated` is required.
    fn wait_until_changed(&self) -> impl Future<Output = SymResult<()>>;

    /// Asyncronously wait until the sensor value has been updated with a value that satisfies a condition. This call **will** update the observer's version
    /// and evaluate the condition function on values obtained by `pull_updated`.
    #[inline]
    fn wait_for<F: Send + FnMut(&<Self::Lock as ReadGuardSpecifier>::Target) -> bool>(
        &self,
        mut condition: F,
    ) -> impl Future<Output = SymResult<<Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>>> {
        async move {
            let mut is_closed = self.is_locally_closed();
            loop {
                let guard = self.pull();
                if condition(&guard) {
                    // Should get compiled into a value assignment.
                    return if is_closed { Err(guard) } else { Ok(guard) };
                }
                drop(guard);
                // Should get compiled into a value assignment.
                is_closed = match self.wait_until_changed().await {
                    Ok(()) => false,
                    Err(()) => true,
                }
            }
        }
    }
}

/// The generalized sensor observer.
pub struct SensorObserver<T, R, L, E = L>
where
    E: ExecManager<L>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
{
    inner: R,
    version: UnsafeCell<usize>,
}

impl<T, R, L, E> Clone for SensorObserver<T, R, L, E>
where
    E: ExecManager<L>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            version: UnsafeCell::new(unsafe { *self.version.get() }),
        }
    }
}

impl<T, R, L, E> SensorObserve for SensorObserver<T, R, L, E>
where
    E: ExecManager<L>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
{
    type Lock = L;

    #[inline(always)]
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.inner.data.0.read()
    }

    #[inline(always)]
    fn pull(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        let guard = self.inner.data.0.read();
        self.mark_seen();
        // unsafe {
        //     *self.version.get() = self.inner.version();
        // }
        guard
    }

    #[inline(always)]
    fn mark_seen(&self) {
        unsafe { *self.version.get() = self.inner.version() };
    }

    #[inline(always)]
    fn mark_unseen(&self) {
        unsafe { *self.version.get() = self.inner.version().wrapping_sub(STEP_SIZE) };
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        let latest_version = self.inner.version();
        let version = unsafe { &mut *self.version.get() };
        *version |= latest_version & CLOSED_BIT;
        *version != latest_version
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        unsafe {
            *self.version.get() |= self.inner.version() & CLOSED_BIT;
        }
        self.is_locally_closed()
    }

    #[inline(always)]
    fn is_locally_closed(&self) -> bool {
        unsafe { *self.version.get() & CLOSED_BIT == CLOSED_BIT }
    }
}

impl<T, R, L, E> SensorObserveAsync for SensorObserver<T, R, L, E>
where
    for<'a> E: ExecManager<L> + ExecRegister<L, &'a Waker>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
{
    async fn wait_until_changed(&self) -> SymResult<()> {
        WaitUntilChangedFuture(Some(self)).await
    }
}

/*** Mapped and Fused Observers ***/

/// A mapped sensor observer.
pub struct MappedSensorObserver<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    /// The original pre-mapped observer.
    pub inner: A,
    map: UnsafeCell<F>,
}

impl<A, T, F> MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    /// Create a new cacheless mapped observer given another observer and an appropriate mapping function.
    #[inline(always)]
    pub const fn map_with(a: A, f: F) -> Self {
        Self {
            inner: a,
            map: UnsafeCell::new(f),
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
    fn pull(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        unsafe { OwnedData((*self.map.get())(&self.inner.pull())) }
    }

    #[inline(always)]
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        unsafe { OwnedData((*self.map.get())(&self.inner.borrow())) }
    }

    #[inline(always)]
    fn mark_seen(&self) {
        self.inner.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&self) {
        self.inner.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.has_changed()
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[inline(always)]
    fn is_locally_closed(&self) -> bool {
        self.inner.is_locally_closed()
    }
}

impl<A, T, F> SensorObserveAsync for MappedSensorObserver<A, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    fn wait_until_changed(&self) -> impl Future<Output = SymResult<()>> {
        self.inner.wait_until_changed()
    }
}

/// A fused sensor observer.
pub struct FusedSensorObserver<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    /// The first original pre-mapped observer.
    pub a: A,
    /// The second original pre-mapped observer.
    pub b: B,
    fuse: UnsafeCell<F>,
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
    /// Create a new cacheless fused observer given two other independent observers and an appropriate fusing function.
    #[inline(always)]
    pub fn fuse_with(a: A, b: B, f: F) -> Self {
        Self {
            a,
            b,
            fuse: UnsafeCell::new(f),
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
    fn pull(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        unsafe { OwnedData((*self.fuse.get())(&self.a.pull(), &self.b.pull())) }
    }

    #[inline(always)]
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        unsafe { OwnedData((*self.fuse.get())(&self.a.borrow(), &self.b.borrow())) }
    }

    #[inline(always)]
    fn mark_seen(&self) {
        self.a.mark_seen();
        self.b.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&self) {
        self.a.mark_unseen();
        self.b.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.a.has_changed() || self.b.has_changed()
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }

    #[inline(always)]
    fn is_locally_closed(&self) -> bool {
        self.a.is_locally_closed() && self.b.is_locally_closed()
    }
}

struct FusedWaitChanged<A, B>
where
    A: Future<Output = SymResult<()>> + Unpin,
    B: Future<Output = SymResult<()>> + Unpin,
{
    a: A,
    b: B,
}

impl<A, B> Future for FusedWaitChanged<A, B>
where
    A: Future<Output = SymResult<()>> + Unpin,
    B: Future<Output = SymResult<()>> + Unpin,
{
    type Output = SymResult<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(res) = pin!(&mut self.a).poll(cx) {
            return Poll::Ready(res);
        }
        if let Poll::Ready(res) = pin!(&mut self.b).poll(cx) {
            return Poll::Ready(res);
        }

        return Poll::Pending;
    }
}

impl<A, B, T, F> SensorObserveAsync for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    B: SensorObserve + SensorObserveAsync,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    async fn wait_until_changed(&self) -> SymResult<()> {
        FusedWaitChanged {
            a: pin!(self.a.wait_until_changed()),
            b: pin!(self.b.wait_until_changed()),
        }
        .await
    }
}

/*** Executables and Async ***/

/// Allows registration of an executable.
pub trait RegisterFunction<F> {
    /// Register an executable.
    fn register(&self, f: F);
}

impl<T, S, L, E, F> RegisterFunction<F> for SensorWriter<T, S, L, E>
where
    L: DataWriteLock<Target = T>,
    E: ExecManager<L> + ExecRegister<L, F>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
{
    fn register(&self, f: F) {
        let inner_data = self.0.share_elided_ref();
        inner_data.1.register(f, &inner_data.0);
    }
}

impl<T, R, L, E, F> RegisterFunction<F> for SensorObserver<T, R, L, E>
where
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
    E: ExecManager<L> + ExecRegister<L, F>,
{
    fn register(&self, f: F) {
        let inner_data = self.inner.share_elided_ref();
        inner_data.1.register(f, &inner_data.0);
    }
}

/// A future that resolves when the sensor value gets updated.
#[repr(transparent)]
pub struct WaitUntilChangedFuture<'a, T, R, L, E>(Option<&'a SensorObserver<T, R, L, E>>)
where
    E: ExecManager<L>,
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>;

impl<'a, T, R, L, E> Future for WaitUntilChangedFuture<'a, T, R, L, E>
where
    L: DataWriteLock<Target = T>,
    R: Deref<Target = RevisedData<(L, E)>>,
    E: ExecManager<L>,
    for<'b> SensorObserver<T, R, L, E>: RegisterFunction<&'b Waker>,
    Self: Unpin,
{
    type Output = Result<(), ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(value) = self.0.take() {
            let has_changed = value.has_changed();
            let closed = value.is_locally_closed();
            if has_changed || closed {
                // Should get compiled out into value assignment
                Poll::Ready(if closed { Err(()) } else { Ok(()) })
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
