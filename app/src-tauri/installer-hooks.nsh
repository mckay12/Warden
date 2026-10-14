; Warden installer hooks (Tauri NSIS). The installer is per-machine (UAC) so it
; can register the Windows service. Every MessageBox carries /SD IDOK so a
; silent (/S) install or uninstall can never hang waiting for a click.

!macro NSIS_HOOK_PREINSTALL
  ; Upgrade over a running service: the exe can't be overwritten while it runs,
  ; so stop it first. While a lock is active the service refuses to stop, which
  ; is exactly when it must not be replaced -- so abort rather than half-install.
  nsExec::ExecToLog 'sc.exe query WardenService'
  Pop $0
  ${If} $0 == 0
    nsExec::ExecToLog 'sc.exe stop WardenService'
    Pop $0
    StrCpy $1 0
    warden_wait_stop:
      nsExec::ExecToLog 'cmd.exe /c sc.exe query WardenService | find "STOPPED"'
      Pop $0
      ${If} $0 == 0
        Goto warden_stopped
      ${EndIf}
      IntOp $1 $1 + 1
      Sleep 1000
      ${If} $1 < 20
        Goto warden_wait_stop
      ${EndIf}
    MessageBox MB_OK|MB_ICONSTOP "Warden can't be updated right now: its background service did not stop. This is expected while a lock is active.$\r$\n$\r$\nWait for the lock to expire, then run the installer again." /SD IDOK
    Abort
    warden_stopped:
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Register (or re-point, on upgrade) and start the service from the install dir.
  nsExec::ExecToLog '"$INSTDIR\warden-service.exe" install'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "Warden's background service could not be set up (exit code $0).$\r$\n$\r$\nIf a lock is currently active, the service can't be replaced until it ends. Otherwise run $\"warden-service.exe install$\" from an administrator prompt in:$\r$\n$INSTDIR" /SD IDOK
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; `uninstall` refuses while a lock is active; honour that by aborting.
  nsExec::ExecToLog '"$INSTDIR\warden-service.exe" uninstall'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Warden can't be uninstalled right now: a lock is active.$\r$\n$\r$\nWait for the lock to expire, then run the uninstaller again." /SD IDOK
    Abort
  ${EndIf}
!macroend
