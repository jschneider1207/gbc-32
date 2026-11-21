//! Hand-written bit sets (the core has no dependencies, so no `bitflags`).

/// Defines a `Copy` bit-set newtype with named flag constants, set
/// operations, and a `Debug` impl that prints flag names.
macro_rules! bitset {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident($ty:ty) {
            $(
                $(#[$flag_meta:meta])*
                const $flag:ident = $value:expr;
            )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
        $vis struct $name($ty);

        impl $name {
            $(
                $(#[$flag_meta])*
                pub const $flag: Self = Self($value);
            )*

            /// No flags set.
            pub const EMPTY: Self = Self(0);
            /// Every defined flag set.
            pub const ALL: Self = Self(0 $(| $value)*);

            /// The raw bits.
            #[inline]
            pub const fn bits(self) -> $ty {
                self.0
            }

            /// Builds a set from raw bits, dropping bits that are not a
            /// defined flag.
            #[inline]
            pub const fn from_bits_truncate(bits: $ty) -> Self {
                Self(bits & Self::ALL.0)
            }

            /// True if no flag is set.
            #[inline]
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }

            /// True if every flag in `other` is also set in `self`.
            #[inline]
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            /// True if `self` and `other` share at least one flag.
            #[inline]
            pub const fn intersects(self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            /// Flags set in either.
            #[inline]
            pub const fn union(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            /// Flags set in both.
            #[inline]
            pub const fn intersection(self, other: Self) -> Self {
                Self(self.0 & other.0)
            }

            /// Flags set in `self` but not in `other`.
            #[inline]
            pub const fn difference(self, other: Self) -> Self {
                Self(self.0 & !other.0)
            }

            /// Sets every flag in `other`.
            #[inline]
            pub fn insert(&mut self, other: Self) {
                self.0 |= other.0;
            }

            /// Clears every flag in `other`.
            #[inline]
            pub fn remove(&mut self, other: Self) {
                self.0 &= !other.0;
            }

            /// Sets or clears every flag in `other`.
            #[inline]
            pub fn set(&mut self, other: Self, value: bool) {
                if value {
                    self.insert(other);
                } else {
                    self.remove(other);
                }
            }
        }

        impl core::ops::BitOr for $name {
            type Output = Self;
            #[inline]
            fn bitor(self, rhs: Self) -> Self {
                self.union(rhs)
            }
        }

        impl core::ops::BitOrAssign for $name {
            #[inline]
            fn bitor_assign(&mut self, rhs: Self) {
                self.insert(rhs);
            }
        }

        impl core::ops::BitAnd for $name {
            type Output = Self;
            #[inline]
            fn bitand(self, rhs: Self) -> Self {
                self.intersection(rhs)
            }
        }

        impl core::ops::BitAndAssign for $name {
            #[inline]
            fn bitand_assign(&mut self, rhs: Self) {
                self.0 &= rhs.0;
            }
        }

        impl core::ops::Sub for $name {
            type Output = Self;
            #[inline]
            fn sub(self, rhs: Self) -> Self {
                self.difference(rhs)
            }
        }

        impl core::ops::Not for $name {
            type Output = Self;
            /// Complement within the defined flags.
            #[inline]
            fn not(self) -> Self {
                Self(!self.0 & Self::ALL.0)
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(concat!(stringify!($name), "("))?;
                let mut first = true;
                $(
                    if self.contains(Self::$flag) {
                        if !first {
                            f.write_str(" | ")?;
                        }
                        f.write_str(stringify!($flag))?;
                        first = false;
                    }
                )*
                let unknown = self.0 & !Self::ALL.0;
                if unknown != 0 {
                    if !first {
                        f.write_str(" | ")?;
                    }
                    write!(f, "{unknown:#x}")?;
                }
                f.write_str(")")
            }
        }
    };
}

pub(crate) use bitset;
