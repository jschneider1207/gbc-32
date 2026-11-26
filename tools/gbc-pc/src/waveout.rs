//! The Win32 side of `play --audio` (ADR 0041 "Amendment: live
//! playback"): the default output device through waveOut (winmm), fed
//! from [`crate::live::Live`] by a playback thread of its own.
//!
//! waveOut over WASAPI: it opens the default device (`WAVE_MAPPER`) in
//! shared mode at any PCM rate (the Windows audio engine converts to the
//! mix rate), takes plain buffers, and needs no COM. Through WSL interop
//! the `.exe` is a native Win32 process, so this is the normal desktop
//! audio path. The rate tried first is 32,768 Hz (the board's, so our
//! resampler runs near 1:1 and only corrects drift), then 48,000 and
//! 44,100 (`WAVE_FORMAT_QUERY`).
//!
//! [`BUFFERS`] buffers of [`BUFFER_MS`] are kept queued; the device signals
//! an auto-reset event (`CALLBACK_EVENT`) as each one finishes, and the
//! thread refills every finished buffer in queue order (waveOut returns
//! them in the order written). All finished at one wake means the device
//! may have run dry: counted as `late`.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Media::Audio::{
    CALLBACK_EVENT, HWAVEOUT, WAVE_FORMAT_PCM, WAVE_FORMAT_QUERY, WAVE_MAPPER, WAVEFORMATEX,
    WAVEHDR, WHDR_DONE, waveOutClose, waveOutOpen, waveOutPrepareHeader, waveOutReset,
    waveOutUnprepareHeader, waveOutWrite,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST, WaitForSingleObject,
};

use crate::live::Live;

/// Device rates, in order of preference.
pub const RATES: [u32; 3] = [32_768, 48_000, 44_100];
pub const BUFFERS: usize = 4;
pub const BUFFER_MS: u32 = 10;

/// `WAVEHDR` is `packed(1)` in windows-sys; this keeps it 8-aligned so
/// `dwFlags` (offset 24) can be read volatile while the driver owns it.
#[repr(C, align(8))]
struct Hdr(WAVEHDR);

fn format(rate: u32) -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 2,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * 4,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

fn mm(what: &str, r: u32) -> io::Result<()> {
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("{what} failed (MMRESULT {r})")))
    }
}

/// The first of [`RATES`] the default device takes.
pub fn query_rate() -> io::Result<u32> {
    for rate in RATES {
        let f = format(rate);
        // SAFETY: a query opens nothing (null handle allowed with
        // WAVE_FORMAT_QUERY); `f` lives across the call.
        let r = unsafe {
            waveOutOpen(
                std::ptr::null_mut(),
                WAVE_MAPPER,
                &f,
                0,
                0,
                WAVE_FORMAT_QUERY,
            )
        };
        if r == 0 {
            return Ok(rate);
        }
    }
    Err(io::Error::other(
        "no audio output device takes 16-bit stereo PCM at 32,768, 48,000 or 44,100 Hz",
    ))
}

/// The running playback thread.
pub struct LiveAudio {
    pub rate: u32,
    stop: Arc<AtomicBool>,
    late: Arc<AtomicU32>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl LiveAudio {
    /// Opens the device and starts the thread; `make` builds the shared
    /// state for the chosen rate.
    pub fn start(
        make: impl FnOnce(u32) -> Arc<Mutex<Live>>,
    ) -> io::Result<(Self, Arc<Mutex<Live>>)> {
        let rate = query_rate()?;
        let live = make(rate);
        let stop = Arc::new(AtomicBool::new(false));
        let late = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let (l, s, k) = (live.clone(), stop.clone(), late.clone());
        let thread = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || run(rate, &l, &s, &k, tx))
            .map_err(io::Error::other)?;
        match rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    rate,
                    stop,
                    late,
                    thread: Some(thread),
                },
                live,
            )),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(match thread.join() {
                Ok(Err(e)) => e,
                _ => io::Error::other("the audio thread ended at start"),
            }),
        }
    }

    /// Times every queued buffer had finished at one wake.
    pub fn late(&self) -> u32 {
        self.late.load(Ordering::Relaxed)
    }

    /// Stops the device and joins the thread.
    pub fn stop(mut self) -> io::Result<()> {
        self.halt()
    }

    fn halt(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.take().map(|t| t.join()) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(io::Error::other("the audio thread panicked")),
            None => Ok(()),
        }
    }
}

impl Drop for LiveAudio {
    fn drop(&mut self) {
        let _ = self.halt();
    }
}

struct Device {
    h: HWAVEOUT,
    event: HANDLE,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: `h` and `event` were opened by `run` and are closed only
        // here; headers are unprepared by `run` before this drops.
        unsafe {
            waveOutClose(self.h);
            CloseHandle(self.event);
        }
    }
}

fn run(
    rate: u32,
    live: &Mutex<Live>,
    stop: &AtomicBool,
    late: &AtomicU32,
    started: mpsc::Sender<io::Result<()>>,
) -> io::Result<()> {
    // SAFETY: plain calls on this thread's own pseudo-handle and a new
    // unnamed auto-reset event.
    let event = unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        CreateEventW(std::ptr::null(), 0, 0, std::ptr::null())
    };
    if event.is_null() {
        let e = io::Error::last_os_error();
        let _ = started.send(Err(io::Error::new(e.kind(), format!("CreateEventW: {e}"))));
        return Ok(());
    }
    let f = format(rate);
    let mut h: HWAVEOUT = std::ptr::null_mut();
    // SAFETY: `h` and `f` outlive the call; the event handle is valid.
    let r = unsafe { waveOutOpen(&mut h, WAVE_MAPPER, &f, event as usize, 0, CALLBACK_EVENT) };
    if let Err(e) = mm("waveOutOpen", r) {
        // SAFETY: the event was created above and is not used again.
        unsafe { CloseHandle(event) };
        let _ = started.send(Err(e));
        return Ok(());
    }
    let dev = Device { h, event };
    let frames = (rate * BUFFER_MS / 1000) as usize;
    let mut data: Vec<Vec<i16>> = (0..BUFFERS).map(|_| vec![0i16; frames * 2]).collect();
    let mut hdrs: Box<[Hdr]> = (0..BUFFERS)
        .map(|_| Hdr(WAVEHDR::default()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let size = size_of::<WAVEHDR>() as u32;
    for (hd, d) in hdrs.iter_mut().zip(&mut data) {
        hd.0.lpData = d.as_mut_ptr().cast();
        hd.0.dwBufferLength = (d.len() * 2) as u32;
        // SAFETY: the header and its buffer live in `hdrs`/`data`, which
        // are neither moved nor dropped until after the unprepare below.
        mm("waveOutPrepareHeader", unsafe {
            waveOutPrepareHeader(dev.h, &mut hd.0, size)
        })?;
    }
    let _ = started.send(Ok(()));

    let result = (|| -> io::Result<()> {
        // Pre-fill: the device starts once the ring holds the target.
        while !stop.load(Ordering::Relaxed) {
            if live.lock().unwrap().ready() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let write = |i: usize, hdrs: &mut [Hdr], data: &mut [Vec<i16>]| -> io::Result<()> {
            live.lock().unwrap().fill(&mut data[i]);
            // SAFETY: header `i` is prepared and not queued (done or new).
            mm("waveOutWrite", unsafe {
                waveOutWrite(dev.h, &mut hdrs[i].0, size)
            })
        };
        if !stop.load(Ordering::Relaxed) {
            for i in 0..BUFFERS {
                write(i, &mut hdrs, &mut data)?;
            }
        }
        let mut next = 0;
        while !stop.load(Ordering::Relaxed) {
            // SAFETY: a valid event handle.
            unsafe { WaitForSingleObject(dev.event, 50) };
            let mut done = 0;
            loop {
                let p = &raw const hdrs[next].0.dwFlags;
                // SAFETY: `p` points into `hdrs` (8-aligned `Hdr`, field
                // at offset 24); the driver sets the flag from another
                // thread, hence the volatile read.
                let flags = unsafe { p.read_volatile() };
                if flags & WHDR_DONE == 0 {
                    break;
                }
                write(next, &mut hdrs, &mut data)?;
                next = (next + 1) % BUFFERS;
                done += 1;
                if done == BUFFERS {
                    late.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
        }
        Ok(())
    })();

    // SAFETY: reset returns every queued header (marked done); then each
    // is unprepared before `hdrs`/`data` drop, and `dev` closes after.
    unsafe {
        waveOutReset(dev.h);
        for hd in hdrs.iter_mut() {
            waveOutUnprepareHeader(dev.h, &mut hd.0, size);
        }
    }
    drop(dev);
    drop(data);
    result
}
