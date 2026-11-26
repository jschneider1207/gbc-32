//! A Windows COM port as a [`Transport`], straight through Win32
//! (`windows-sys`; plan D9). 8N1, no flow control, binary mode.
//!
//! Reads use the documented "return what is there, else wait for the
//! first byte" timeout mode (Microsoft Learn, `COMMTIMEOUTS`): with
//! `ReadIntervalTimeout` and `ReadTotalTimeoutMultiplier` both `MAXDWORD`
//! and `0 < ReadTotalTimeoutConstant < MAXDWORD`, `ReadFile` returns at
//! once with the buffered bytes, or with the first byte to arrive, or
//! with none after `ReadTotalTimeoutConstant` ms.

use std::io;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Devices::Communication::{
    COMMTIMEOUTS, DCB, GetCommState, NOPARITY, ONESTOPBIT, PURGE_RXABORT, PURGE_RXCLEAR,
    PURGE_TXABORT, PURGE_TXCLEAR, PurgeComm, SetCommState, SetCommTimeouts,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, ReadFile, WriteFile,
};

use crate::transport::Transport;

const MAXDWORD: u32 = u32::MAX;
/// How long one `WriteFile` may take: far more than a largest frame
/// (1,040 bytes, ~90 ms at 115,200 baud) needs.
const WRITE_TIMEOUT_MS: u32 = 10_000;

// DCB bit fields (`_bitfield`), Win32 `DCB` layout from bit 0: fBinary,
// fParity, fOutxCtsFlow, fOutxDsrFlow, fDtrControl (2 bits),
// fDsrSensitivity, fTXContinueOnXoff, fOutX, fInX, fErrorChar, fNull,
// fRtsControl (2 bits), fAbortOnError. fNull must stay 0: it would drop
// the 0x00 frame delimiters.
const F_BINARY: u32 = 1 << 0;
const DTR_CONTROL_ENABLE: u32 = 1 << 4;
const RTS_CONTROL_ENABLE: u32 = 1 << 12;

pub struct ComPort {
    h: HANDLE,
    /// The read timeout currently set, in ms.
    read_ms: u32,
}

impl ComPort {
    /// Opens `name` (`COM5`, or a full `\\.\COM5` path) at `baud`, 8N1,
    /// and discards anything already queued in either direction.
    pub fn open(name: &str, baud: u32) -> io::Result<Self> {
        let path = if name.starts_with(r"\\.\") {
            name.to_string()
        } else {
            format!(r"\\.\{name}")
        };
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives
        // the call; a null security-attributes pointer and a null template
        // handle are both documented as allowed.
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            return Err(io::Error::new(e.kind(), format!("cannot open {path}: {e}")));
        }
        // From here `Drop` closes the handle on any error.
        let mut port = Self { h, read_ms: 0 };

        let mut dcb = DCB {
            DCBlength: size_of::<DCB>() as u32,
            ..DCB::default()
        };
        // SAFETY: `port.h` is an open comm handle and `dcb` a valid,
        // writable DCB with `DCBlength` set.
        check(unsafe { GetCommState(port.h, &mut dcb) }, "GetCommState")?;
        dcb.BaudRate = baud;
        dcb._bitfield = F_BINARY | DTR_CONTROL_ENABLE | RTS_CONTROL_ENABLE;
        dcb.ByteSize = 8;
        dcb.Parity = NOPARITY;
        dcb.StopBits = ONESTOPBIT;
        // SAFETY: as above; the DCB is only read.
        check(unsafe { SetCommState(port.h, &dcb) }, "SetCommState")?;
        port.set_read_timeout(1000)?;
        let all = PURGE_RXABORT | PURGE_RXCLEAR | PURGE_TXABORT | PURGE_TXCLEAR;
        // SAFETY: `port.h` is an open comm handle; the flags are valid
        // PURGE_* values.
        let ok = unsafe { PurgeComm(port.h, all) };
        check(ok, "PurgeComm")?;
        Ok(port)
    }

    fn set_read_timeout(&mut self, ms: u32) -> io::Result<()> {
        // 0 and MAXDWORD change the mode (see the module docs).
        let ms = ms.clamp(1, MAXDWORD - 1);
        if ms == self.read_ms {
            return Ok(());
        }
        let t = COMMTIMEOUTS {
            ReadIntervalTimeout: MAXDWORD,
            ReadTotalTimeoutMultiplier: MAXDWORD,
            ReadTotalTimeoutConstant: ms,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: WRITE_TIMEOUT_MS,
        };
        // SAFETY: `self.h` is an open comm handle; `t` is a valid
        // COMMTIMEOUTS that is only read.
        check(unsafe { SetCommTimeouts(self.h, &t) }, "SetCommTimeouts")?;
        self.read_ms = ms;
        Ok(())
    }
}

fn check(ok: i32, what: &str) -> io::Result<()> {
    if ok != 0 {
        Ok(())
    } else {
        let e = io::Error::last_os_error();
        Err(io::Error::new(e.kind(), format!("{what}: {e}")))
    }
}

impl Transport for ComPort {
    fn write_all(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let len = data.len().min(u32::MAX as usize) as u32;
            let mut written = 0u32;
            // SAFETY: `data` is valid for `len` bytes of reads and
            // `written` for a u32 write; the handle is open and not
            // overlapped, so a null OVERLAPPED pointer is correct.
            let ok =
                unsafe { WriteFile(self.h, data.as_ptr(), len, &mut written, ptr::null_mut()) };
            check(ok, "WriteFile")?;
            if written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "WriteFile wrote nothing (write timeout)",
                ));
            }
            data = &data[written as usize..];
        }
        Ok(())
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(MAXDWORD);
        self.set_read_timeout(ms)?;
        let len = buf.len().min(u32::MAX as usize) as u32;
        let mut read = 0u32;
        // SAFETY: `buf` is valid for `len` bytes of writes and `read` for
        // a u32 write; the handle is open and not overlapped, so a null
        // OVERLAPPED pointer is correct.
        let ok = unsafe { ReadFile(self.h, buf.as_mut_ptr(), len, &mut read, ptr::null_mut()) };
        check(ok, "ReadFile")?;
        Ok(read as usize)
    }
}

impl Drop for ComPort {
    fn drop(&mut self) {
        // SAFETY: `self.h` is the handle `CreateFileW` returned, closed
        // exactly once, here.
        unsafe {
            CloseHandle(self.h);
        }
    }
}
