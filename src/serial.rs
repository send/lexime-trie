use crate::{DoubleArray, Label, LemmaError};

impl<L: Label> DoubleArray<L> {
    /// Serializes the double-array trie to a byte vector.
    pub fn as_bytes(&self) -> Vec<u8> {
        let _ = &self.nodes;
        todo!("as_bytes will be implemented in feat/serialization")
    }

    /// Deserializes a double-array trie from a byte slice.
    pub fn from_bytes(_bytes: &[u8]) -> Result<Self, LemmaError> {
        todo!("from_bytes will be implemented in feat/serialization")
    }
}
