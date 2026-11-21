//! Audio processing unit. Skeleton only: arrives in M6, which also fixes
//! the output sample rate.

/// One stereo output sample, written by the core into the caller's audio
/// buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StereoSample {
    pub left: i16,
    pub right: i16,
}
