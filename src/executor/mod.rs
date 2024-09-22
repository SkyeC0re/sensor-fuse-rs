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
    /// Execute all executables registered on the executor by giving it access to a write guard through a commit function.
    /// The executor must call the commit function once it is ready to commit to the next execution cycle, such that
    /// it becomes impossible to register additional executables for the newly initiated execution cycle.
    fn execute<'a, C: FnOnce() -> L::WriteGuard<'a>>(&'a self, commit: C)
    where
        L: 'a;
}

/// Signifies that an execution manager may register an executable using immutable access.
pub trait ExecRegister<L, F>: ExecManager<L>
where
    L: DataWriteLock,
{
    /// Register an executable unit on the executor's execution set. The executor calls `C` and registers `F` if `Some(F)`
    /// is returned, and returns true if `F` was registered. The executor must guarantee that the evaluation and registration step is atomic
    /// with respect to `commit` functions.
    fn register<C: FnOnce() -> Option<F>>(&self, condition: C) -> bool;
}

impl<L> ExecManager<L> for ()
where
    L: DataWriteLock,
{
    #[inline(always)]
    fn execute<'a, F: FnOnce() -> L::WriteGuard<'a>>(&'a self, commit: F)
    where
        L: 'a,
    {
        let _ = commit();
    }
}
