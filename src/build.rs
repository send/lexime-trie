use crate::{CodeMapper, DoubleArray, Label};

impl<L: Label> DoubleArray<L> {
    /// Builds a double-array trie from sorted keys.
    ///
    /// Each key `keys[i]` is assigned `value_id = i`.
    ///
    /// # Panics
    /// - If keys are not sorted in ascending order.
    pub fn build(_keys: &[impl AsRef<[L]>]) -> Self {
        let _ = CodeMapper::build::<L>(&[]);
        todo!("build will be implemented in feat/build")
    }
}
