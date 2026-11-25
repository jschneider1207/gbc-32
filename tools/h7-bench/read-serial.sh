#!/usr/bin/env bash
# Prints what the board sends on the ST-LINK virtual COM port for N seconds
# (default 10). 115200 8N1; read-only. README.md "Read the output".
#
#   ./read-serial.sh [seconds]
#
#   H7_COM   the Windows port (default COM3, "STMicroelectronics STLink
#            Virtual COM Port" in Device Manager)
set -euo pipefail

secs=${1:-10}
port=${H7_COM:-COM3}
case "$secs" in ''|*[!0-9]*) echo "usage: read-serial.sh [seconds]" >&2; exit 2;; esac
case "$port" in COM[0-9]|COM[0-9][0-9]) ;; *) echo "read-serial.sh: bad port $port" >&2; exit 2;; esac

# The port is opened through .NET from Windows PowerShell: WSL2 has no
# access to COM ports of its own.
powershell.exe -NoProfile -NonInteractive -Command "
  \$ErrorActionPreference = 'Stop'
  \$p = New-Object System.IO.Ports.SerialPort '$port', 115200, 'None', 8, 'One'
  \$p.ReadTimeout = 200
  try {
    \$p.Open()
    \$end = (Get-Date).AddSeconds($secs)
    while ((Get-Date) -lt \$end) {
      \$s = \$p.ReadExisting()
      if (\$s.Length -gt 0) { [Console]::Out.Write(\$s) } else { Start-Sleep -Milliseconds 50 }
    }
  } finally {
    if (\$p.IsOpen) { \$p.Close() }
  }
" | tr -d '\r'
