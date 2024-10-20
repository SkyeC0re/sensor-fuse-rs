// #[cfg(feature = "alloc")]
// pub mod standard;
#[cfg(feature = "alloc")]
pub type BoxedFn<T> = Box<dyn 'static + Send + FnMut(&T) -> bool>;
/// An execution manager that only requires immutable access.
pub trait ExecManager<T> {
    /// Execute all executables registered on the executor.
    fn execute(&self, value: &T);
}

impl<T> ExecManager<T> for () {
    #[inline(always)]
    fn execute(&self, _: &T) {}
}
