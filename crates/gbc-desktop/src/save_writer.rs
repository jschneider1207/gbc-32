//! Writes save files on a background thread (ADR 0023 amendment), so a
//! slow disk, an antivirus scan or `sync_all` never stalls the frame loop.
//!
//! The main thread hands over owned snapshots of the encoded file. One
//! thread writes them in order; a snapshot submitted while another waits
//! replaces it (the newer one has everything the older one had), so the
//! last snapshot always wins and none is written out of order. `finish`
//! queues the exit save, then joins the thread: when it returns, every
//! snapshot that will ever be written has been.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Save files written since the last [`SaveWriter::take_stats`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WriteStats {
    pub count: u64,
    /// The longest single write.
    pub max: Duration,
}

#[derive(Default)]
struct State {
    /// The newest snapshot not yet being written.
    pending: Option<Vec<u8>>,
    /// A write is in progress.
    busy: bool,
    /// No more snapshots: write what is pending, then exit.
    closing: bool,
    /// A write failed since the last `take_failed`.
    failed: bool,
    stats: WriteStats,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // The lock is never held across a write or anything that panics;
        // if the writer died anyway, the state is still usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct SaveWriter {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl SaveWriter {
    /// A writer for `path` that writes with `save::write_atomic`.
    pub fn spawn(path: PathBuf) -> io::Result<Self> {
        Self::spawn_with(path, crate::save::write_atomic)
    }

    /// A writer that writes with `write` (tests stub it out).
    pub fn spawn_with<F>(path: PathBuf, mut write: F) -> io::Result<Self>
    where
        F: FnMut(&Path, &[u8]) -> io::Result<()> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("save-writer".into())
            .spawn(move || {
                // The result of the last write: after `finish`, the exit save's.
                let mut last = Ok(());
                loop {
                    let data = {
                        let mut st = s.lock();
                        while st.pending.is_none() && !st.closing {
                            st = s.changed.wait(st).unwrap_or_else(PoisonError::into_inner);
                        }
                        match st.pending.take() {
                            Some(d) => {
                                st.busy = true;
                                d
                            }
                            None => return last,
                        }
                    };
                    let t = Instant::now();
                    let r = write(&path, &data);
                    let took = t.elapsed();
                    let mut st = s.lock();
                    st.busy = false;
                    st.stats.count += 1;
                    st.stats.max = st.stats.max.max(took);
                    if let Err(e) = &r {
                        st.failed = true;
                        // During play the caller retries; the exit save's
                        // error is returned by `finish` and reported there.
                        if !st.closing {
                            eprintln!("warning: can't write {}: {e}", path.display());
                        }
                    }
                    s.changed.notify_all();
                    drop(st);
                    last = r;
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Queues `data` to be written, replacing any snapshot still waiting.
    /// Never blocks on the disk.
    pub fn submit(&self, data: Vec<u8>) {
        self.shared.lock().pending = Some(data);
        self.shared.changed.notify_all();
    }

    /// Whether a write failed since the last call.
    pub fn take_failed(&self) -> bool {
        std::mem::take(&mut self.shared.lock().failed)
    }

    /// Writes finished since the last call.
    pub fn take_stats(&self) -> WriteStats {
        std::mem::take(&mut self.shared.lock().stats)
    }

    /// Blocks until nothing is pending or being written.
    #[cfg(test)]
    pub fn wait_idle(&self) {
        let mut st = self.shared.lock();
        while st.pending.is_some() || st.busy {
            st = self
                .shared
                .changed
                .wait(st)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Queues `last` (the exit save) if given, waits for every queued write
    /// and stops the thread. Returns the last write's result: with `last`,
    /// whether the exit save reached the disk.
    pub fn finish(mut self, last: Option<Vec<u8>>) -> io::Result<()> {
        self.close(last)
    }

    fn close(&mut self, last: Option<Vec<u8>>) -> io::Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        {
            let mut st = self.shared.lock();
            if last.is_some() {
                st.pending = last;
            }
            st.closing = true;
        }
        self.shared.changed.notify_all();
        thread
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the save writer thread panicked")))
    }
}

impl Drop for SaveWriter {
    /// Not finished (an early return): still write what was queued.
    fn drop(&mut self) {
        let _ = self.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A writer whose writes report each snapshot on `written` and then
    /// wait for a token on `go` (or fail when the snapshot starts with 0xEE).
    fn gated() -> (SaveWriter, mpsc::Receiver<Vec<u8>>, mpsc::Sender<()>) {
        let (wtx, wrx) = mpsc::channel();
        let (gtx, grx) = mpsc::channel::<()>();
        let w = SaveWriter::spawn_with(PathBuf::from("unused"), move |_, d| {
            wtx.send(d.to_vec()).unwrap();
            grx.recv().unwrap();
            if d.first() == Some(&0xEE) {
                Err(io::Error::other("disk full"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        (w, wrx, gtx)
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gbc-writer-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn snapshots_queued_during_a_write_coalesce_to_the_newest() {
        let (w, written, go) = gated();
        w.submit(vec![1]);
        assert_eq!(written.recv().unwrap(), [1], "first write started");
        // Queued while 1 is being written: only the newest is written.
        w.submit(vec![2]);
        w.submit(vec![3]);
        w.submit(vec![4]);
        go.send(()).unwrap();
        assert_eq!(written.recv().unwrap(), [4]);
        go.send(()).unwrap();
        w.finish(None).unwrap();
        assert!(written.try_recv().is_err(), "nothing else was written");
    }

    #[test]
    fn finish_writes_the_exit_save_after_the_write_in_flight() {
        let (w, written, go) = gated();
        w.submit(vec![1]);
        assert_eq!(written.recv().unwrap(), [1]);
        w.submit(vec![2]); // superseded by the exit save
        let shared = w.shared.clone();
        let finishing = std::thread::spawn(move || w.finish(Some(vec![9])));
        // Release the write in flight only once the exit save is queued.
        while !shared.lock().closing {
            std::thread::yield_now();
        }
        go.send(()).unwrap();
        go.send(()).unwrap();
        finishing.join().unwrap().unwrap();
        let rest: Vec<_> = written.try_iter().collect();
        assert_eq!(rest, [vec![9]], "the exit save, last and once");
    }

    #[test]
    fn exit_save_reaches_the_disk_before_finish_returns() {
        let dir = tmp_dir("exit");
        let p = dir.join("a.sav");
        let w = SaveWriter::spawn(p.clone()).unwrap();
        for i in 0..50u8 {
            w.submit(vec![i; 1024]);
        }
        w.finish(Some(vec![0xAB; 1024])).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), vec![0xAB; 1024]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dropping_an_unfinished_writer_still_writes_the_queued_snapshot() {
        let dir = tmp_dir("drop");
        let p = dir.join("a.sav");
        let w = SaveWriter::spawn(p.clone()).unwrap();
        w.submit(vec![7; 16]);
        drop(w);
        assert_eq!(std::fs::read(&p).unwrap(), vec![7; 16]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failures_are_reported_and_the_exit_save_result_returned() {
        let (w, written, go) = gated();
        w.submit(vec![0xEE]);
        go.send(()).unwrap();
        written.recv().unwrap();
        w.wait_idle();
        assert!(w.take_failed(), "a failed write is reported");
        assert!(!w.take_failed(), "once");
        w.submit(vec![1]);
        go.send(()).unwrap();
        written.recv().unwrap();
        w.wait_idle();
        assert!(!w.take_failed(), "a good write is not a failure");
        assert_eq!(w.take_stats().count, 2);
        assert_eq!(w.take_stats().count, 0, "stats restart");
        // An exit save that fails is an error from finish...
        go.send(()).unwrap();
        assert!(w.finish(Some(vec![0xEE])).is_err());

        // ...and one that succeeds after an earlier failure is not.
        let (w, _written, go) = gated();
        w.submit(vec![0xEE]);
        go.send(()).unwrap();
        go.send(()).unwrap();
        w.finish(Some(vec![1])).unwrap();
    }

    #[test]
    fn submit_does_not_wait_for_a_slow_write() {
        let (w, written, go) = gated();
        w.submit(vec![1]);
        written.recv().unwrap(); // the write is now blocked on `go`
        let t = Instant::now();
        w.submit(vec![2]);
        assert!(!w.take_failed());
        let _ = w.take_stats();
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "{:?}",
            t.elapsed()
        );
        go.send(()).unwrap();
        go.send(()).unwrap();
        w.finish(None).unwrap();
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    #[test]
    fn wait_idle_waits_for_the_write_in_flight() {
        let (wtx, wrx) = mpsc::channel();
        let (gtx, grx) = mpsc::channel::<()>();
        let w = SaveWriter::spawn_with(PathBuf::from("unused"), move |_, _| {
            wtx.send(()).unwrap();
            grx.recv().unwrap();
            Ok(())
        })
        .unwrap();
        w.submit(vec![1]);
        wrx.recv().unwrap(); // taken off the queue, blocked mid-write
        let idle = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                w.wait_idle();
                idle.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(50));
            let early = idle.load(Ordering::SeqCst);
            // Release the write before asserting, so a failure can't hang.
            gtx.send(()).unwrap();
            assert!(!early, "returned mid-write");
        });
        assert!(idle.load(Ordering::SeqCst));
        w.finish(None).unwrap();
    }
}
