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
    ops::{Deref, DerefMut},
    pin::pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};
use derived_deref::{Deref, DerefMut};
use executor::ExecManager;
use futures::{future::Select, select, task::UnsafeFutureObj, FutureExt};
use sensor_core::{SensorCore, SensorCoreAsync};
use std::{marker::PhantomData, mem::MaybeUninit, pin::Pin};

use crate::lock::{DataReadLock, DataWriteLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier};

pub type SymResult<T> = Result<T, T>;

const STATUS_SUCCESS_BIT: u8 = 1;
const STATUS_CLOSED_BIT: u8 = 2;

#[repr(transparent)]
#[derive(Clone)]
pub struct ObservationStatus(u8);

impl ObservationStatus {
    #[inline(always)]
    pub const fn new() -> Self {
        Self(0)
    }

    #[inline(always)]
    pub const fn closed(&self) -> bool {
        self.0 & STATUS_CLOSED_BIT > 0
    }

    #[inline(always)]
    pub const fn success(&self) -> bool {
        self.0 & STATUS_SUCCESS_BIT > 0
    }

    #[inline(always)]
    pub(crate) fn modify_closed(mut self, closed: bool) -> Self {
        self.0 = self.0 & (!STATUS_CLOSED_BIT);
        self.0 |= STATUS_CLOSED_BIT & ((closed as u8) << 1);
        self
    }

    #[inline(always)]
    pub(crate) fn modify_success(mut self, success: bool) -> Self {
        self.0 = self.0 & (!STATUS_SUCCESS_BIT);
        self.0 |= STATUS_SUCCESS_BIT & success as u8;
        self
    }

    #[inline(always)]
    pub(crate) fn set_closed(mut self) -> Self {
        self.0 |= STATUS_CLOSED_BIT;
        self
    }

    #[inline(always)]
    pub(crate) fn set_success(mut self) -> Self {
        self.0 |= STATUS_SUCCESS_BIT;
        self
    }
}

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
// pub trait DerefSensorData: Deref<Target = RawSensorData<Self::Core, Self::Executor>> {
//     /// The data type that the sensor stores.
//     type Target;
//     /// The locking mechanism for the sensor's data.
//     type Core: SensorCore<Target = <Self as DerefSensorData>::Target>;
//     /// The executor associated with the sensor.
//     type Executor: ExecManager<<Self as DerefSensorData>::Target>;
// }

// impl<C, E, R> DerefSensorData for R
// where
//     C: SensorCore,
//     E: ExecManager<C::Target>,
//     R: Deref<Target = RawSensorData<C, E>>,
// {
//     type Target = C::Target;
//     type Core = C;
//     type Executor = E;
// }

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

// impl<T, S> Drop for SensorWriter<T, S>
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = T>,
// {
//     fn drop(&mut self) {
//         let data = <&S as ShareStrategy<'_>>::share_elided_ref(&self.0);
//         data.core.deregister_writer();
//     }
// }

// impl<T, S> Clone for SensorWriter<T, S>
// where
//     S: Clone,
//     for<'a> &'a S: ShareStrategy<'a, Target = T>,
// {
//     fn clone(&self) -> Self {
//         let _ = self
//             .0
//             .share_elided_ref()
//             .writers
//             .fetch_add(1, Ordering::Relaxed);
//         Self(self.0.clone())
//     }
// }

// impl<T, S> SensorWriter<T, S>
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = T>,
// {
//     /// Acquire a read lock on the underlying data.
//     #[inline(always)]
//     pub fn read(&self) -> <<&S as ShareStrategy<'_>>::Core as SensorCore>::ReadGuard<'_> {
//         self.0.share_elided_ref().lock.read()
//     }

//     /// Acquire a write lock on the underlying data.
//     #[inline(always)]
//     pub fn write(&self) -> <<&S as ShareStrategy<'_>>::Core as SensorCore>::WriteGuard<'_> {
//         self.0.share_elided_ref().lock.write()
//     }

//     /// Update the sensor value, notify observers and execute all registered callbacks.
//     #[inline(always)]
//     pub fn update(&self, sample: T) {
//         self.modify_with(|v| *v = sample)
//     }

//     /// Modify the sensor value in place, notify observers and execute all registered callbacks.
//     #[inline]
//     pub fn modify_with(&self, f: impl FnOnce(&mut T)) {
//         let sensor_state = self.0.share_elided_ref();
//         let mut guard = sensor_state.lock.write();

//         // Atomic downgrade just occurred. No other modification can happen.
//         sensor_state.executor.execute(|| {
//             f(&mut guard);
//             let _ = sensor_state.version.fetch_add(STEP_SIZE, Ordering::Release);
//             guard
//         });
//     }

//     /// Mark the current sensor value as unseen to all observers, notify them and execute all registered callbacks.
//     #[inline(always)]
//     pub fn mark_all_unseen(&self) {
//         self.modify_with(|_| ())
//     }

//     /// Spawn an observer by immutably borrowing the sensor writer's data. By definition this observer's scope will be limited
//     /// by the scope of the writer.
//     #[inline(always)]
//     pub fn spawn_referenced_observer(
//         &self,
//     ) -> SensorObserver<
//         T,
//         &'_ RawSensorData<<&S as ShareStrategy<'_>>::Core, <&S as ShareStrategy<'_>>::Executor>,
//     > {
//         let inner = self.0.share_elided_ref();
//         let _ = inner.observers.fetch_add(1, Ordering::Relaxed);
//         SensorObserver {
//             inner,
//             version: UnsafeCell::new(inner.version.load(Ordering::Acquire)),
//         }
//     }

//     /// Spawn an observer by leveraging the sensor writer's sharing strategy. May allow the observer
//     /// to outlive the sensor writer for an appropriate sharing strategy (such as if the writer wraps its data in an `Arc`).
//     #[inline(always)]
//     pub fn spawn_observer(&self) -> SensorObserver<T, <&'_ S as ShareStrategy>::Shared> {
//         let inner = self.0.share_data();
//         let _ = inner.observers.fetch_add(1, Ordering::Relaxed);
//         SensorObserver {
//             version: UnsafeCell::new(inner.version.load(Ordering::Acquire)),
//             inner,
//         }
//     }
// }

/*** Sensor Observation ***/

/// General observer functionality.
pub trait SensorObserve {
    type Target;
    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;
    type Checkpoint: VersionFunctionality + Clone + Unpin;

    fn save_checkpoint(&self) -> Self::Checkpoint;

    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint);

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
    fn wait_until_changed(&self) -> impl Future<Output = (Self::Checkpoint, ObservationStatus)>;

    fn wait_for_and_map<'a, O, M: FnMut(Self::ReadGuard<'a>) -> (O, bool)>(
        &'a mut self,
        condition_map: M,
        check_current: bool,
    ) -> impl Future<Output = (O, ObservationStatus)>
    where
        Self: 'a;

    /// Asynchronously wait until the sensor value has been updated with a value that satisfies a condition. This call **will** update the observer's version
    /// and evaluate the condition function on values obtained by `pull_updated`.
    fn wait_for<F: FnMut(&Self::Target) -> bool>(
        &mut self,
        mut condition: F,
        check_current: bool,
    ) -> impl Future<Output = (Self::ReadGuard<'_>, ObservationStatus)> {
        self.wait_for_and_map(
            move |guard| {
                let success = condition(&guard);
                (guard, success)
            },
            check_current,
        )
    }
}

#[repr(transparent)]
#[derive(Clone)]
pub struct Version(usize);

pub trait VersionFunctionality {
    fn closed(&self) -> bool;
}

impl VersionFunctionality for Version {
    #[inline(always)]
    fn closed(&self) -> bool {
        self.0 & CLOSED_BIT > 0
    }
}

/// The generalized sensor observer.
pub struct SensorObserver<T, C, R>
where
    C: SensorCore<Target = T>,
    R: Deref<Target = C>,
{
    inner: R,
    version: UnsafeCell<usize>,
}

impl<T, C, R> SensorObserve for SensorObserver<T, C, R>
where
    C: SensorCore<Target = T>,
    R: Deref<Target = C>,
{
    type Target = T;
    type ReadGuard<'read> = C::ReadGuard<'read> where Self: 'read;

    type Checkpoint = Version;

    #[inline(always)]
    fn save_checkpoint(&self) -> Self::Checkpoint {
        Version(unsafe { *self.version.get() })
    }

    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint) {
        *self.version.get_mut() = checkpoint.0;
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        *self.version.get_mut() = self.inner.version();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        *self.version.get_mut() = self.inner.version().wrapping_sub(STEP_SIZE);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.version() != unsafe { *self.version.get() }
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.version() & CLOSED_BIT > 0
    }
}

impl<T, C, R> SensorObserveAsync for SensorObserver<T, C, R>
where
    C: SensorCoreAsync<Target = T> + SensorObserve<Target = T>,
    R: Deref<Target = C>,
{
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> {
        self.inner.read()
    }

    fn wait_until_changed(&self) -> impl Future<Output = (Version, ObservationStatus)> {
        FutureExt::map(
            self.inner.wait_changed(unsafe { *self.version.get() }),
            |(version, status)| (Version(version), status),
        )
    }

    fn wait_for_and_map<'a, O, M: FnMut(Self::ReadGuard<'a>) -> (O, bool)>(
        &'a mut self,
        condition_map: M,
        check_current: bool,
    ) -> impl Future<Output = (O, ObservationStatus)> {
        async move {
            let (latest_version, mapped, status) = self
                .inner
                .wait_for_and_map(
                    condition_map,
                    if check_current {
                        None
                    } else {
                        Some(*self.version.get_mut())
                    },
                )
                .await;
            *self.version.get_mut() = latest_version;
            (mapped, status)
        }
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
    type Checkpoint = A::Checkpoint;

    #[inline(always)]
    fn save_checkpoint(&self) -> Self::Checkpoint {
        self.inner.save_checkpoint()
    }

    #[inline(always)]
    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint) {
        self.inner.restore_checkpoint(checkpoint);
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

    fn wait_until_changed(&self) -> impl Future<Output = (Self::Checkpoint, ObservationStatus)> {
        self.inner.wait_until_changed()
    }

    fn wait_for_and_map<'a, O, M: FnMut(Self::ReadGuard<'a>) -> (O, bool)>(
        &'a mut self,
        mut condition_map: M,
        check_current: bool,
    ) -> impl Future<Output = (O, ObservationStatus)>
    where
        Self: 'a,
    {
        let MappedSensorObserver { inner, map } = self;
        inner.wait_for_and_map(
            move |guard| condition_map(OwnedData((map.get_mut())(&guard))),
            check_current,
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

#[derive(Clone)]
pub struct FusedVersion<A: VersionFunctionality, B: VersionFunctionality> {
    a: A,
    b: B,
}

impl<A: VersionFunctionality, B: VersionFunctionality> VersionFunctionality for FusedVersion<A, B> {
    #[inline]
    fn closed(&self) -> bool {
        self.a.closed() && self.b.closed()
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
    type Checkpoint = FusedVersion<A::Checkpoint, B::Checkpoint>;

    #[inline]
    fn save_checkpoint(&self) -> Self::Checkpoint {
        FusedVersion {
            a: self.a.save_checkpoint(),
            b: self.b.save_checkpoint(),
        }
    }

    #[inline]
    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint) {
        self.a.restore_checkpoint(&checkpoint.a);
        self.b.restore_checkpoint(&checkpoint.b);
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
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }
}

struct FusedWaitChangedFut<
    A: Future<Output = (CA, ObservationStatus)>,
    CA: VersionFunctionality + Clone + Unpin,
    B: Future<Output = (CB, ObservationStatus)>,
    CB: VersionFunctionality + Clone + Unpin,
    F: Fn() -> bool,
> {
    a: MaybeUninit<A>,
    // Checkpoint for a.
    ca: CA,
    a_valid: bool,
    // Checkpoint for b.
    b: B,
    cb: CB,
    b_closed: F,
}

impl<
        A: Future<Output = (CA, ObservationStatus)>,
        CA: VersionFunctionality + Clone + Unpin,
        B: Future<Output = (CB, ObservationStatus)>,
        CB: VersionFunctionality + Clone + Unpin,
        F: Unpin + Fn() -> bool,
    > Future for FusedWaitChangedFut<A, CA, B, CB, F>
{
    type Output = (FusedVersion<CA, CB>, ObservationStatus);

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let Self {
            a,
            ca,
            a_valid,
            b,
            cb,
            b_closed,
        } = unsafe { self.get_unchecked_mut() };

        if *a_valid {
            if let Poll::Ready((checkpoint_a, status)) =
                unsafe { Pin::new_unchecked(a.assume_init_mut()).poll(cx) }
            {
                if status.success() {
                    let closed = status.closed() && b_closed();
                    return Poll::Ready((
                        FusedVersion {
                            a: checkpoint_a,
                            b: cb.clone(),
                        },
                        status.modify_closed(closed),
                    ));
                }

                *ca = checkpoint_a;

                unsafe {
                    a.assume_init_drop();
                }
                *a_valid = false;
            }
        }

        // From here it is known that `A` is closed, therefore `B`'s result can be propogated without alteration.
        unsafe { Pin::new_unchecked(b) }
            .poll(cx)
            .map(|(checkpoint_b, status)| {
                (
                    FusedVersion {
                        a: ca.clone(),
                        b: checkpoint_b,
                    },
                    status,
                )
            })
    }
}

impl<A, B, T, F> SensorObserveAsync for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    B: SensorObserve + SensorObserveAsync,
    F: FnMut(&A::Target, &B::Target) -> T,
{
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> {
        async move {
            let a = self.a.read().await;
            let b = self.b.read().await;
            OwnedData(unsafe { (*self.fuse.get())(&*a, &*b) })
        }
    }

    fn wait_until_changed(&self) -> impl Future<Output = (Self::Checkpoint, ObservationStatus)> {
        let Self { a, b, .. } = self;
        FusedWaitChangedFut {
            a: MaybeUninit::new(a.wait_until_changed()),
            ca: a.save_checkpoint(),
            a_valid: true,
            b: self.b.wait_until_changed(),
            cb: b.save_checkpoint(),
            b_closed: move || b.is_closed(),
        }
    }

    fn wait_for_and_map<'a, O, M: FnMut(Self::ReadGuard<'a>) -> (O, bool)>(
        &'a mut self,
        mut condition_map: M,
        check_current: bool,
    ) -> impl Future<Output = (O, ObservationStatus)>
    where
        Self: 'a,
    {
        let checkpoint = self.save_checkpoint();
        let mut data = DropFn::new((self, checkpoint), |(s, cp)| s.restore_checkpoint(cp));
        async move {
            let (s, restore_checkpoint) = &mut *data;
            if !check_current {
                let _ = s.wait_until_changed().await;
            }
            loop {
                let guard_a = s.a.read().await;
                let checkpoint_a = s.a.save_checkpoint();

                let guard_b = s.b.read().await;
                let checkpoint_b = s.b.save_checkpoint();

                let fused_checkpoint = FusedVersion {
                    a: checkpoint_a,
                    b: checkpoint_b,
                };

                let (mapped, success) =
                    condition_map(OwnedData(s.fuse.get_mut()(&guard_a, &guard_b)));

                if success {
                    *restore_checkpoint = fused_checkpoint;
                    return (
                        mapped,
                        ObservationStatus::new()
                            .set_success()
                            .modify_closed(restore_checkpoint.closed()),
                    );
                }

                drop(guard_a);
                drop(guard_b);

                s.restore_checkpoint(&fused_checkpoint);
                let _ = s.wait_until_changed().await;
            }
        }
    }
}

struct DropFn<T, F: FnOnce(&mut T)> {
    v: T,
    f: MaybeUninit<F>,
}

impl<T, F: FnOnce(&mut T)> DropFn<T, F> {
    #[inline(always)]
    pub const fn new(value: T, drop_fn: F) -> Self {
        Self {
            v: value,
            f: MaybeUninit::new(drop_fn),
        }
    }
}

impl<T, F: FnOnce(&mut T)> Drop for DropFn<T, F> {
    #[inline]
    fn drop(&mut self) {
        let Self { v, f } = self;

        unsafe { (f.assume_init_read())(v) };
    }
}

impl<T, F: FnOnce(&mut T)> Deref for DropFn<T, F> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.v
    }
}

impl<T, F: FnOnce(&mut T)> DerefMut for DropFn<T, F> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.v
    }
}
