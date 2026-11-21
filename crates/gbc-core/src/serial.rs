//! Serial port (SB/SC). Skeleton only: transfers arrive in M1.

pub(crate) struct Serial {
    /// The most recent byte sent out (reported with `Events::SERIAL_TX`).
    pub(crate) last_byte: u8,
}

impl Serial {
    pub(crate) const fn new() -> Self {
        Self { last_byte: 0 }
    }
}
