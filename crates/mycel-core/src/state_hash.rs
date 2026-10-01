//! Canonical SHA-256 state encoding and hashing.
//!
//! Implementations must emit fields in a fixed order and iterate collections in
//! deterministic order. All integer encodings are explicitly little-endian and
//! type-tagged; strings/byte arrays/sequences are length-prefixed.

use sha2::{Digest, Sha256};

/// Streaming canonical encoder for authoritative simulation state.
pub struct CanonicalWriter {
    hasher: Sha256,
}

impl CanonicalWriter {
    /// Starts a new version-1 authoritative-state encoding.
    #[must_use]
    pub fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"mycel-authoritative-state-v1\0");
        Self { hasher }
    }

    /// Encodes a boolean with a type tag.
    pub fn write_bool(&mut self, value: bool) {
        self.hasher.update([0x01, u8::from(value)]);
    }

    /// Encodes an unsigned 8-bit integer with a type tag.
    pub fn write_u8(&mut self, value: u8) {
        self.hasher.update([0x02, value]);
    }

    /// Encodes an unsigned 16-bit integer in little-endian order.
    pub fn write_u16(&mut self, value: u16) {
        self.hasher.update([0x03]);
        self.hasher.update(value.to_le_bytes());
    }

    /// Encodes an unsigned 32-bit integer in little-endian order.
    pub fn write_u32(&mut self, value: u32) {
        self.hasher.update([0x04]);
        self.hasher.update(value.to_le_bytes());
    }

    /// Encodes an unsigned 64-bit integer in little-endian order.
    pub fn write_u64(&mut self, value: u64) {
        self.hasher.update([0x05]);
        self.hasher.update(value.to_le_bytes());
    }

    /// Encodes a signed 16-bit integer in little-endian two's-complement order.
    pub fn write_i16(&mut self, value: i16) {
        self.hasher.update([0x06]);
        self.hasher.update(value.to_le_bytes());
    }

    /// Encodes a signed 32-bit integer in little-endian two's-complement order.
    pub fn write_i32(&mut self, value: i32) {
        self.hasher.update([0x07]);
        self.hasher.update(value.to_le_bytes());
    }

    /// Encodes a signed 64-bit integer in little-endian two's-complement order.
    pub fn write_i64(&mut self, value: i64) {
        self.hasher.update([0x08]);
        self.hasher.update(value.to_le_bytes());
    }

    /// Encodes a length-prefixed byte sequence.
    pub fn write_bytes(&mut self, value: &[u8]) {
        self.hasher.update([0x09]);
        self.write_length(value.len());
        self.hasher.update(value);
    }

    /// Encodes a length-prefixed UTF-8 string.
    pub fn write_str(&mut self, value: &str) {
        self.hasher.update([0x0a]);
        self.write_length(value.len());
        self.hasher.update(value.as_bytes());
    }

    /// Encodes a sequence length. Call this before writing its elements.
    pub fn write_sequence_len(&mut self, len: usize) {
        self.hasher.update([0x0b]);
        self.write_length(len);
    }

    fn write_length(&mut self, len: usize) {
        // Every supported Rust target has usize no wider than u128.
        #[allow(clippy::cast_lossless)]
        let len = len as u128;
        self.hasher.update(len.to_le_bytes());
    }

    fn finish(self) -> StateHash {
        let digest = self.hasher.finalize();
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(&digest);
        StateHash(bytes)
    }
}

impl Default for CanonicalWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// SHA-256 digest of one canonical authoritative state encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StateHash([u8; 32]);

impl StateHash {
    /// Raw SHA-256 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase 64-character hexadecimal representation.
    #[must_use]
    pub fn to_hex(self) -> String {
        use std::fmt::Write as _;

        let mut output = String::with_capacity(64);
        for byte in self.0 {
            // Writing into String is infallible.
            let _ = write!(output, "{byte:02x}");
        }
        output
    }
}

impl std::fmt::Display for StateHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Contract for state types that can emit a stable canonical representation.
///
/// Implementations must write fields in a fixed documented order; collections
/// must be sorted or have an explicitly deterministic sequence order. Never
/// encode memory addresses, hash-map iteration order, wall-clock values, floats,
/// or backend state.
pub trait CanonicalState {
    /// Emits this value's authoritative fields.
    fn write_canonical(&self, writer: &mut CanonicalWriter);

    /// Computes a versioned SHA-256 digest of this value.
    #[must_use]
    fn state_hash(&self) -> StateHash {
        let mut writer = CanonicalWriter::new();
        self.write_canonical(&mut writer);
        writer.finish()
    }
}

macro_rules! impl_canonical_integer {
    ($type:ty, $method:ident) => {
        impl CanonicalState for $type {
            fn write_canonical(&self, writer: &mut CanonicalWriter) {
                writer.$method(*self);
            }
        }
    };
}

impl_canonical_integer!(bool, write_bool);
impl_canonical_integer!(u8, write_u8);
impl_canonical_integer!(u16, write_u16);
impl_canonical_integer!(u32, write_u32);
impl_canonical_integer!(u64, write_u64);
impl_canonical_integer!(i16, write_i16);
impl_canonical_integer!(i32, write_i32);
impl_canonical_integer!(i64, write_i64);

impl CanonicalState for String {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_str(self);
    }
}

impl<T: CanonicalState> CanonicalState for Option<T> {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        match self {
            Some(value) => {
                writer.write_u8(1);
                value.write_canonical(writer);
            }
            None => writer.write_u8(0),
        }
    }
}

impl<T: CanonicalState> CanonicalState for Vec<T> {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_sequence_len(self.len());
        for value in self {
            value.write_canonical(writer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CanonicalState, CanonicalWriter};

    #[test]
    fn hash_is_stable_type_tagged_and_sensitive_to_field_order() {
        assert_eq!(42_u64.state_hash(), 42_u64.state_hash());
        assert_ne!(42_u64.state_hash(), 42_u32.state_hash());
        assert_ne!(vec![1_u32, 2].state_hash(), vec![2_u32, 1].state_hash());
    }

    #[test]
    fn length_prefix_prevents_ambiguous_byte_sequences() {
        let mut first = CanonicalWriter::new();
        first.write_bytes(&[1, 2]);
        let mut second = CanonicalWriter::new();
        second.write_bytes(&[1]);
        second.write_bytes(&[2]);
        assert_ne!(first.finish(), second.finish());
    }

    #[test]
    fn hash_hex_is_lowercase_fixed_width() {
        let hash = 1_u8.state_hash();
        assert_eq!(hash.to_hex().len(), 64);
        assert_eq!(hash.to_hex(), hash.to_string());
        assert!(
            hash.to_hex()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }
}
