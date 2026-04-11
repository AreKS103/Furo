; Furo NSIS Installer Hooks
; Kills running Furo and whisper-server processes before installing,
; preventing "Error opening file for writing" on locked DLLs.

!macro NSIS_HOOK_PREINSTALL
  ; Kill whisper-server first (child process that locks DLLs)
  nsExec::ExecToLog 'taskkill /F /IM "whisper-server.exe"'
  ; Kill Furo main process
  nsExec::ExecToLog 'taskkill /F /IM "Furo.exe"'
  ; Brief pause to let OS release file handles
  Sleep 1000
!macroend
