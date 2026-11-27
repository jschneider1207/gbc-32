//! The byte link the client runs over: a COM port on Windows
//! (`crate::serial`), a simulated board in tests (`crate::sim`, behind
//! the `sim` feature).

use std::io;
use std::time::Duration;

pub trait Transport {
    /// Sends every byte of `data`, or fails.
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;

    /// Waits up to `timeout` for data and reads what has arrived into
    /// `buf`, returning as soon as anything is there (it need not fill
    /// `buf`). `Ok(0)` means nothing arrived within `timeout`.
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).write_all(data)
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        (**self).read_timeout(buf, timeout)
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).write_all(data)
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        (**self).read_timeout(buf, timeout)
    }
}

/// `--port null`: no board. Writes go nowhere and reads wait out their
/// timeout with nothing, so `play --port null --seconds N` exercises the
/// keyboard and the loop without opening a COM port.
pub struct NullPort;

impl Transport for NullPort {
    fn write_all(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read_timeout(&mut self, _buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        std::thread::sleep(timeout);
        Ok(0)
    }
}
