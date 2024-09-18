use crate::lock::DataWriteLock;

#[cfg(feature = "alloc")]
pub mod standard;

#[cfg(feature = "alloc")]
pub type BoxedFn<T> = Box<dyn 'static + Send + FnMut(&T) -> bool>;
/// An execution manager that only requires immutable access.
pub trait ExecManager<L>
where
    L: DataWriteLock,
{
    /// Execute all executables registered on the executor manager by giving it access to an atomically downgraded write guard. This will
    /// either just be the write guard or a read guard that was atomically downgraded from a write guard. As such the execution
    /// manager is given the guarantee that as long as it holds the guard, no other `execute` call will be initiated.
    fn execute(&self, value: L::DowngradedGuard<'_>);
}

/// Signifies that an execution manager may register an executable using immutable access.
pub trait ExecRegister<L, F>: ExecManager<L>
where
    L: DataWriteLock,
{
    /// Register an executable unit on the executor's execution set. The executor must call `C` and register `F` if `Some(F)`
    /// is returned, and returns true if `F` was registered. If the executor supports simultaneous execution
    /// and registration then it must guarantee that the next execution cycle contains `F` if it was registered.
    fn register<C: FnOnce() -> Option<F>>(&self, condition: C, lock: &L) -> bool;
}

impl<L> ExecManager<L> for ()
where
    L: DataWriteLock,
{
    #[inline(always)]
    fn execute(&self, _: L::DowngradedGuard<'_>) {}
}
