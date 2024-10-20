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
//! an executable can be anything but fundamentally represents a piece of work that must occur each time the sensor value is updated. The framework's basic
//! non-trivial executor (`struct@StdExec`) for example allows for both the registration of `Waker`s to power asynchronous behaviour and
//! callback functions that act on references to the sensor value. Each time the sensor value is updated, `struct@StdExec` will execute all `Waker`s and
//! callbacks, dropping all the Wakers and those callbacks that produced `false` as output values.
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
pub mod sensor_core;

#[cfg(feature = "alloc")]
use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    future::{poll_fn, Future},
    ops::Deref,
    pin::pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};
use derived_deref::{Deref, DerefMut};
use executor::ExecManager;
use futures::{future::Select, FutureExt};
use sensor_core::{SensorCore, SensorCoreAsync};
use std::marker::PhantomData;

use crate::lock::{DataReadLock, DataWriteLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier};

pub type SymResult<T> = Result<T, T>;

/*** Sensor State ***/

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If it's not broken don't fix it.
const CLOSED_BIT: usize = 1;
const STEP_SIZE: usize = 2;

/// Data structure for storing data with an atomic version tag.
pub struct RawSensorData<C, E>
where
    C: SensorCore,
    E: ExecManager<C::Target>,
{
    core: C,
    executor: E,
}

impl<C, E> RawSensorData<C, E>
where
    C: SensorCore,
    E: ExecManager<C::Target>,
{
    #[inline(always)]
    const fn new(core: C, executor: E) -> Self {
        Self { core, executor }
    }
}
/// Trait for sharing a (most likely heap pointer) wrapped `struct@RawSensorData` with a locking strategy.
pub trait ShareStrategy<'a> {
    /// The data type that the sensor stores.
    type Target;
    /// The locking mechanism for the sensor's data.
    type Core: SensorCore<Target = Self::Target>;
    /// The executor associated with the sensor.
    type Executor: ExecManager<Self::Target>;
    /// The wrapping container for the sensor state.
    type Shared: Deref<Target = RawSensorData<Self::Core, Self::Executor>>;

    /// Share the sensor state for the largest possible lifetime.
    fn share_data(self) -> Self::Shared;

    /// Create an immutable borrow to the sensor state, elided by the lifetime of
    /// wrapping container.
    fn share_elided_ref(self) -> &'a RawSensorData<Self::Core, Self::Executor>;
}

impl<'a, C, E> ShareStrategy<'a> for &'a RawSensorData<C, E>
where
    C: SensorCore,
    E: ExecManager<C::Target>,
{
    type Target = C::Target;
    type Core = C;
    type Executor = E;
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

/// QOL trait to encapsulate sensor observer's data wrapper, locking strategy and executor type.
pub trait DerefSensorData: Deref<Target = RawSensorData<Self::Core, Self::Executor>> {
    /// The data type that the sensor stores.
    type Target;
    /// The locking mechanism for the sensor's data.
    type Core: SensorCore<Target = <Self as DerefSensorData>::Target>;
    /// The executor associated with the sensor.
    type Executor: ExecManager<<Self as DerefSensorData>::Target>;
}

impl<C, E, R> DerefSensorData for R
where
    C: SensorCore,
    E: ExecManager<C::Target>,
    R: Deref<Target = RawSensorData<C, E>>,
{
    type Target = C::Target;
    type Core = C;
    type Executor = E;
}

#[cfg(feature = "alloc")]
impl<'a, C, E> ShareStrategy<'a> for &'a Arc<RawSensorData<C, E>>
where
    C: SensorCore,
    E: ExecManager<C::Target>,
{
    type Target = C::Target;
    type Core = C;
    type Executor = E;
    type Shared = Arc<RawSensorData<C, E>>;

    #[inline(always)]
    fn share_data(self) -> Self::Shared {
        self.clone()
    }

    #[inline(always)]
    fn share_elided_ref(self) -> &'a RawSensorData<C, E> {
        self
    }
}

/*** Sensor Writing ***/

/// The generalized sensor writer.
#[repr(transparent)]
pub struct SensorWriter<T, S>(S)
where
    for<'a> &'a S: ShareStrategy<'a, Target = T>;

impl<C, E> SensorWriter<C::Target, RawSensorData<C, E>>
where
    C: SensorCore,
    E: ExecManager<C::Target>,
{
    /// Create a new sensor writer by wrapping the appropriate shared data.
    #[inline(always)]
    pub const fn from_parts(core: C, executor: E) -> Self {
        Self(RawSensorData::new(core, executor))
    }
}

#[cfg(feature = "alloc")]
impl<C, E> SensorWriter<C::Target, Arc<RawSensorData<C, E>>>
where
    C: SensorCore,
    E: ExecManager<C::Target>,
{
    /// Create a new sensor writer by wrapping the appropriate shared data.
    #[inline(always)]
    pub fn from_parts_arc(core: C, executor: E) -> Self {
        Self(Arc::new(RawSensorData::new(core, executor)))
    }
}

impl<T, S> Drop for SensorWriter<T, S>
where
    for<'a> &'a S: ShareStrategy<'a, Target = T>,
{
    fn drop(&mut self) {
        let data = <&S as ShareStrategy<'_>>::share_elided_ref(&self.0);
        if data.writers.fetch_sub(1, Ordering::Relaxed) == 1 {
            let _ = data.version.fetch_or(CLOSED_BIT, Ordering::Release);
        }
    }
}

impl<T, S> Clone for SensorWriter<T, S>
where
    S: Clone,
    for<'a> &'a S: ShareStrategy<'a, Target = T>,
{
    fn clone(&self) -> Self {
        let _ = self
            .0
            .share_elided_ref()
            .writers
            .fetch_add(1, Ordering::Relaxed);
        Self(self.0.clone())
    }
}

impl<T, S> SensorWriter<T, S>
where
    for<'a> &'a S: ShareStrategy<'a, Target = T>,
{
    /// Acquire a read lock on the underlying data.
    #[inline(always)]
    pub fn read(&self) -> <<&S as ShareStrategy<'_>>::Core as SensorCore>::ReadGuard<'_> {
        self.0.share_elided_ref().lock.read()
    }

    /// Acquire a write lock on the underlying data.
    #[inline(always)]
    pub fn write(&self) -> <<&S as ShareStrategy<'_>>::Core as SensorCore>::WriteGuard<'_> {
        self.0.share_elided_ref().lock.write()
    }

    /// Update the sensor value, notify observers and execute all registered callbacks.
    #[inline(always)]
    pub fn update(&self, sample: T) {
        self.modify_with(|v| *v = sample)
    }

    /// Modify the sensor value in place, notify observers and execute all registered callbacks.
    #[inline]
    pub fn modify_with(&self, f: impl FnOnce(&mut T)) {
        let sensor_state = self.0.share_elided_ref();
        let mut guard = sensor_state.lock.write();

        // Atomic downgrade just occurred. No other modification can happen.
        sensor_state.executor.execute(|| {
            f(&mut guard);
            let _ = sensor_state.version.fetch_add(STEP_SIZE, Ordering::Release);
            guard
        });
    }

    /// Mark the current sensor value as unseen to all observers, notify them and execute all registered callbacks.
    #[inline(always)]
    pub fn mark_all_unseen(&self) {
        self.modify_with(|_| ())
    }

    /// Spawn an observer by immutably borrowing the sensor writer's data. By definition this observer's scope will be limited
    /// by the scope of the writer.
    #[inline(always)]
    pub fn spawn_referenced_observer(
        &self,
    ) -> SensorObserver<
        T,
        &'_ RawSensorData<<&S as ShareStrategy<'_>>::Core, <&S as ShareStrategy<'_>>::Executor>,
    > {
        let inner = self.0.share_elided_ref();
        let _ = inner.observers.fetch_add(1, Ordering::Relaxed);
        SensorObserver {
            inner,
            version: UnsafeCell::new(inner.version.load(Ordering::Acquire)),
        }
    }

    /// Spawn an observer by leveraging the sensor writer's sharing strategy. May allow the observer
    /// to outlive the sensor writer for an appropriate sharing strategy (such as if the writer wraps its data in an `Arc`).
    #[inline(always)]
    pub fn spawn_observer(&self) -> SensorObserver<T, <&'_ S as ShareStrategy>::Shared> {
        let inner = self.0.share_data();
        let _ = inner.observers.fetch_add(1, Ordering::Relaxed);
        SensorObserver {
            version: UnsafeCell::new(inner.version.load(Ordering::Acquire)),
            inner,
        }
    }
}

/*** Sensor Observation ***/

/// General observer functionality.
pub trait SensorObserve {
    type Target;
    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;

    fn mark_seen(&mut self);
    /// Mark the current sensor data as unseen.
    fn mark_unseen(&mut self);
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
    /// - `function@has_changed`
    /// - `function@is_closed`
    /// - `function@wait_until_changed` is awaited.
    /// - `function@wait_for` is awaited.
    // fn is_locally_closed(&self) -> bool;

    /// Fuse this observer with another using the given fusion function into a cacheless observer.
    #[inline(always)]
    fn fuse<B, T, F>(self, other: B, f: F) -> FusedSensorObserver<Self, B, T, F>
    where
        Self: Sized,
        B: SensorObserve,
        F: FnMut(&<Self as SensorObserve>::Target, &<B as SensorObserve>::Target) -> T,
    {
        FusedSensorObserver::fuse_with(self, other, f)
    }

    /// Map this observer into another using the given mapping function into a cacheless observer.
    #[inline(always)]
    fn map<T, F>(self, f: F) -> MappedSensorObserver<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<Self as SensorObserve>::Target) -> T,
    {
        MappedSensorObserver::map_with(self, f)
    }
}

/// Async observer functionality.
pub trait SensorObserveAsync: SensorObserve {
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>>;

    /// Asynchronously wait until the sensor value is updated. This call will **not** update the observer's version,
    /// as such an additional call to `pull` or `pull_updated` is required.
    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>>;

    /// Asynchronously wait until the sensor value has been updated with a value that satisfies a condition. This call **will** update the observer's version
    /// and evaluate the condition function on values obtained by `pull_updated`.
    fn wait_for<'a, C: 'a>(
        &'a mut self,
        condition: C,
    ) -> impl Future<Output = SymResult<Self::ReadGuard<'a>>>
    where
        for<'b> C: FnMut(&'b Self::Target) -> bool;
}

/// The generalized sensor observer.
pub struct SensorObserver<T, R>
where
    R: DerefSensorData<Target = T>,
{
    inner: R,
    version: UnsafeCell<usize>,
}

impl<T, R> SensorObserve for SensorObserver<T, R>
where
    R: DerefSensorData<Target = T>,
{
    type Target = T;
    type ReadGuard<'read> = <R::Core as SensorCore>::ReadGuard<'read> where Self: 'read;

    #[inline(always)]
    fn mark_seen(&mut self) {
        *self.version.get_mut() = self.inner.core.version();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        *self.version.get_mut() = self.inner.core.version().wrapping_sub(STEP_SIZE);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.core.version() != unsafe { *self.version.get() }
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.core.version() & CLOSED_BIT > 0
    }
}

impl<T, R> SensorObserveAsync for SensorObserver<T, R>
where
    R: DerefSensorData<Target = T>,
    R::Core: SensorCoreAsync,
{
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> {
        self.inner.core.read()
    }

    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>> {
        FutureExt::map(
            self.inner.core.wait_changed(unsafe { *self.version.get() }),
            |res| if res.is_ok() { Ok(()) } else { Err(()) },
        )
    }

    fn wait_for<'a, C: 'a>(
        &'a mut self,
        condition: C,
    ) -> impl Future<Output = SymResult<Self::ReadGuard<'a>>>
    where
        for<'b> C: FnMut(&'b Self::Target) -> bool,
    {
        FutureExt::map(
            self.inner.core.wait_for(condition, None),
            |(res, latest_version)| {
                *self.version.get_mut() = latest_version;
                res
            },
        )
    }
}

/*** Mapped and Fused Observers ***/

/// A mapped sensor observer.
pub struct MappedSensorObserver<A: SensorObserve, T, F: FnMut(&A::Target) -> T> {
    /// The original pre-mapped observer.
    pub inner: A,
    map: UnsafeCell<F>,
}

impl<A, T, F> MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&A::Target) -> T,
{
    /// Create a new mapped observer given another observer and an appropriate mapping function.
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
    F: FnMut(&A::Target) -> T,
{
    type Target = T;
    type ReadGuard<'read> = OwnedData<T> where Self: 'read;

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
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

impl<A, T, F> SensorObserveAsync for MappedSensorObserver<A, T, F>
where
    A: SensorObserveAsync,
    F: FnMut(&A::Target) -> T,
{
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> {
        FutureExt::map(self.inner.read(), |inner_value| {
            OwnedData(unsafe { (*self.map.get())(&inner_value) })
        })
    }

    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>> {
        self.inner.wait_until_changed()
    }

    fn wait_for<'a, C: 'a>(
        &'a mut self,
        condition: C,
    ) -> impl Future<Output = SymResult<Self::ReadGuard<'_>>>
    where
        for<'b> C: FnMut(&'b Self::Target) -> bool,
    {
        let mut latest_value;
        FutureExt::map(
            self.inner.wait_for(|inner_val| {
                latest_value = OwnedData((self.map.get_mut())(inner_val));
                condition(&latest_value)
            }),
            |res| {
                if res.is_ok() {
                    Ok(latest_value)
                } else {
                    Err(latest_value)
                }
            },
        )
    }
}

/// A fused sensor observer.
pub struct FusedSensorObserver<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&A::Target, &B::Target) -> T,
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
    F: FnMut(&A::Target, &B::Target) -> T,
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
    F: FnMut(&A::Target, &B::Target) -> T,
{
    type Target = T;
    type ReadGuard<'read> = OwnedData<T> where Self: 'read;

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
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }
}

impl<A, B, T, F> SensorObserveAsync for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    B: SensorObserve + SensorObserveAsync,
    F: FnMut(&A::Target, &B::Target) -> T,
{
    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>> {
        // let mut a = self.a.wait_until_changed();
        // let mut b = self.b.wait_until_changed();
        // poll_fn(move |cx| {
        //     if let Poll::Ready(res) = pin!(&mut a).poll(cx) {
        //         return Poll::Ready(res);
        //     }
        //     if let Poll::Ready(res) = pin!(&mut b).poll(cx) {
        //         return Poll::Ready(res);
        //     }

        //     return Poll::Pending;
        // })
        todo!()
    }

    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> {
        todo!()
    }

    fn wait_for<'a, C: 'a + FnMut(&'a Self::Target) -> bool>(
        &'a mut self,
        condition: C,
    ) -> impl Future<Output = SymResult<Self::ReadGuard<'_>>> {
        todo!()
    }
}
