/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Guest pointers and the memory interface. Allocation stays with the host.

use std::ffi::c_void;
use std::marker::PhantomData;

/// Equivalent of `usize` for guest memory (the guest is 32-bit).
pub type GuestUSize = u32;
/// Equivalent of `isize` for guest memory.
pub type GuestISize = i32;
/// An untyped 32-bit guest virtual address.
pub type VAddr = GuestUSize;

/// [std::mem::size_of], but returning a [GuestUSize].
pub const fn guest_size_of<T: Sized>() -> GuestUSize {
    assert!(std::mem::size_of::<T>() <= u32::MAX as usize);
    std::mem::size_of::<T>() as u32
}

/// A guest pointer. `MUT` selects mutability; use the [ConstPtr] / [MutPtr] /
/// [ConstVoidPtr] / [MutVoidPtr] aliases rather than spelling out `MUT`.
///
/// It is just a typed 32-bit address — dereferencing happens through [Mem].
#[repr(transparent)]
pub struct Ptr<T, const MUT: bool>(VAddr, PhantomData<T>);

// Manual impls: derive would (wrongly) require `T: Trait`.
impl<T, const MUT: bool> Clone for Ptr<T, MUT> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T, const MUT: bool> Copy for Ptr<T, MUT> {}
impl<T, const MUT: bool> PartialEq for Ptr<T, MUT> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<T, const MUT: bool> Eq for Ptr<T, MUT> {}
impl<T, const MUT: bool> std::hash::Hash for Ptr<T, MUT> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// Constant guest pointer (like Rust's `*const T`).
pub type ConstPtr<T> = Ptr<T, false>;
/// Mutable guest pointer (like Rust's `*mut T`).
pub type MutPtr<T> = Ptr<T, true>;
/// Constant guest pointer-to-void (like C's `const void *`).
pub type ConstVoidPtr = ConstPtr<c_void>;
/// Mutable guest pointer-to-void (like C's `void *`).
pub type MutVoidPtr = MutPtr<c_void>;

impl<T, const MUT: bool> Ptr<T, MUT> {
    pub const fn null() -> Self {
        Ptr(0, PhantomData)
    }
    pub fn to_bits(self) -> VAddr {
        self.0
    }
    pub const fn from_bits(bits: VAddr) -> Self {
        Ptr(bits, PhantomData)
    }
    pub fn cast<U>(self) -> Ptr<U, MUT> {
        Ptr::<U, MUT>::from_bits(self.to_bits())
    }
    pub fn cast_void(self) -> Ptr<c_void, MUT> {
        self.cast()
    }
    pub fn is_null(self) -> bool {
        self.to_bits() == 0
    }
}

impl<T> ConstPtr<T> {
    pub fn cast_mut(self) -> MutPtr<T> {
        Ptr::from_bits(self.to_bits())
    }
}
impl<T> MutPtr<T> {
    pub fn cast_const(self) -> ConstPtr<T> {
        Ptr::from_bits(self.to_bits())
    }
}

impl<T, const MUT: bool> Default for Ptr<T, MUT> {
    fn default() -> Self {
        Self::null()
    }
}

impl<T, const MUT: bool> std::fmt::Debug for Ptr<T, MUT> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_null() {
            write!(f, "(null)")
        } else {
            write!(f, "{:#x}", self.to_bits())
        }
    }
}

// C-like pointer arithmetic (scaled by the pointee size).
impl<T, const MUT: bool> std::ops::Add<GuestUSize> for Ptr<T, MUT> {
    type Output = Self;
    fn add(self, other: GuestUSize) -> Self {
        let size: GuestUSize = guest_size_of::<T>();
        assert_ne!(size, 0);
        Self::from_bits(
            self.to_bits()
                .checked_add(other.checked_mul(size).unwrap())
                .unwrap(),
        )
    }
}
impl<T, const MUT: bool> std::ops::AddAssign<GuestUSize> for Ptr<T, MUT> {
    fn add_assign(&mut self, rhs: GuestUSize) {
        *self = *self + rhs;
    }
}
impl<T, const MUT: bool> std::ops::Sub<GuestUSize> for Ptr<T, MUT> {
    type Output = Self;
    fn sub(self, other: GuestUSize) -> Self {
        let size: GuestUSize = guest_size_of::<T>();
        assert_ne!(size, 0);
        Self::from_bits(
            self.to_bits()
                .checked_sub(other.checked_mul(size).unwrap())
                .unwrap(),
        )
    }
}
impl<T, const MUT: bool> std::ops::SubAssign<GuestUSize> for Ptr<T, MUT> {
    fn sub_assign(&mut self, rhs: GuestUSize) {
        *self = *self - rhs;
    }
}

/// Guest page size (iPhone OS used 4 KiB).
pub const PAGE_SIZE: GuestUSize = 4096;

/// The memory interface the interpreter depends on. [Mem] is the default
/// implementation; implement this yourself to use a custom backing store.
///
/// All accessors are byte-oriented and bounds-checked; the host defines data null-access policy. `addr` below is a
/// raw guest address wrapped in a [ConstVoidPtr] / [MutPtr]; use
/// [`Ptr::from_bits`] / [`Ptr::to_bits`] to convert.
pub trait GuestMem {
    /// Bytes below this address are the "null segment"; any access there is a
    /// instruction-fetch fault. Data reads/writes follow the accessors below.
    fn null_segment_size(&self) -> VAddr;
    /// Read-only view of `count` bytes at `addr`, or [None] if out of bounds or
    /// in the null segment.
    fn get_bytes_fallible(&self, addr: ConstVoidPtr, count: GuestUSize) -> Option<&[u8]>;
    /// Mutable view of `count` bytes at `addr`, or [None] if out of bounds or in
    /// the null segment.
    fn get_bytes_fallible_mut(
        &mut self,
        addr: ConstVoidPtr,
        count: GuestUSize,
    ) -> Option<&mut [u8]>;
    /// Mutable view of `count` bytes; **panics** on a null-segment access (the
    /// infallible primitive, mirroring touchHLE's `bytes_at_mut`).
    fn bytes_at_mut(&mut self, ptr: MutPtr<u8>, count: GuestUSize) -> &mut [u8];
}
