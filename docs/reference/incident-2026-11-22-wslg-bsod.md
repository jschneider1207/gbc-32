# Incident 2026-11-22: WSLg window run → Windows host BSOD

Why AGENTS.md ("Machine resources") forbids GUI and GPU use from WSL.

## Sequence
1. The human ran the Linux `gbc-desktop` window on Pokémon Crystal under
   WSLg. WSLg's compositor (weston) died with SIGSEGV at 22:08:32
   (`/mnt/wslg/stderr.log`). The app reported `Io error: Broken pipe (os
   error 32)` / `event loop failed` (its Wayland socket closed). weston had
   also segfaulted twice at 20:43 during an agent's audio runs.
2. The orchestrating agent re-ran the window to reproduce it, without
   asking. weston segfaulted again at 22:10:32.
3. Shortly after, the Windows host blue-screened.

The same ROM ran fine as a native Windows build. Headless Linux runs were
fine too. Why Crystal (and not Red) crashes weston is **unknown**.

## Dump analysis (human, WinDbg `!analyze -v` on `C:\Windows\MEMORY.DMP`)
- Bugcheck 0x7E `SYSTEM_THREAD_EXCEPTION_NOT_HANDLED`, exception
  0xC0000005, a read of address 0x15B (`mov cl,[rax+15Bh]` with rax = 0,
  so a NULL object pointer).
- Faulting IP: `dxgmms2!VIDMM_GLOBAL::CreateOneAllocation+0x4f`
  (dxgmms2.sys 10.0.19041.7058, Windows 10 19041, Hyper-V root).
- **PROCESS_NAME: `vmmem`**, the WSL2 VM. The GPU work belonged to the
  WSL VM's GPU paravirtualization (`/dev/dxg`), not to a native Windows
  process.
- Stack (VidMm worker thread): `VidMmWorkerThreadProc` →
  `VIDMM_WORKER_THREAD::Run` → `ProcessDeferredCommand` → `LockInternal` →
  `LockInAperture` → `PageInOneAllocation` → `CommitVirtualAddressRangesInList`
  → `CVirtualAddressAllocator::CommitVirtualAddressRange` →
  `VIDMM_PAGE_DIRECTORY::CommitVirtualAddressRange` (x2) → `CreatePageTable`
  → `VIDMM_PAGE_TABLE::InitializePageTable` → `CreateVidMmObjects` →
  `CreateOneAllocation` (NULL deref).

## Interpretation (inference, not proven)
A deferred page-in or lock of a GPU allocation owned by the WSL VM ran
while the object it depended on was already gone. The likely cause is the
VM-side GPU context being torn down or recreated as weston crashed and
WSLg restarted it, a race in Windows' video memory manager. Our app renders
on the CPU (softbuffer, `wl_shm`). Its only GPU exposure under WSLg is
weston compositing it. So the dangerous path is "WSLg/weston crash + GPU
paravirtualization", which any WSL GUI or GPU program can reach.

## Rules that follow (AGENTS.md)
- No Linux GUI under WSLg, and no GPU use from WSL at all, even headless.
- Native Windows `.exe` runs only through `scripts/run-windows.sh`: short,
  self-closing with `--frames`, on scratch ROM copies.
- Never reproduce a crash in host or desktop components without asking.
- Strongest guard (the human's choice): `guiApplications=false` in
  `%UserProfile%\.wslconfig` disables WSLg entirely.
