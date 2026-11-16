; Nimbus installer hooks (Windows).
;
; Two pieces get registered with the Service Control Manager:
;
;   mullvad-split-tunnel  the signed WFP driver (kernel service)
;   nimbus-split          the LocalSystem agent that owns it and serves the UI
;
; The agent installs itself via `--install`, because its command line contains
; the installation path and getting that through NSIS, sc.exe and the SCM
; without mangling the quotes is where this usually breaks. The driver's path
; lives in System32 and has no spaces, so plain sc.exe is safe there.
;
; Everything is best-effort: a failure to register the services must not abort
; the install. Without them Nimbus still works, it just reports that app split
; tunneling needs its service.

!macro NSIS_HOOK_POSTINSTALL
  Push $0

  ; --- the agent -----------------------------------------------------------
  StrCpy $0 "$INSTDIR\nimbus-split-win.exe"
  ${IfNot} ${FileExists} "$0"
    StrCpy $0 "$INSTDIR\resources\nimbus-split-win.exe"
  ${EndIf}
  ${If} ${FileExists} "$0"
    nsExec::ExecToLog '"$0" --install'
  ${EndIf}

  ; --- the driver ----------------------------------------------------------
  StrCpy $0 "$INSTDIR\mullvad-split-tunnel.sys"
  ${IfNot} ${FileExists} "$0"
    StrCpy $0 "$INSTDIR\resources\mullvad-split-tunnel.sys"
  ${EndIf}
  ${If} ${FileExists} "$0"
    CopyFiles /SILENT "$0" "$SYSDIR\drivers\nimbus-split-tunnel.sys"
    nsExec::ExecToLog 'sc.exe create nimbus-split-tunnel type= kernel start= demand binPath= "$SYSDIR\drivers\nimbus-split-tunnel.sys"'
    nsExec::ExecToLog 'sc.exe start nimbus-split-tunnel'
  ${EndIf}

  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Push $0

  ; Stop the agent first: it holds the driver's exclusive control device, so
  ; stopping the driver while the agent is running would leave it wedged.
  StrCpy $0 "$INSTDIR\nimbus-split-win.exe"
  ${IfNot} ${FileExists} "$0"
    StrCpy $0 "$INSTDIR\resources\nimbus-split-win.exe"
  ${EndIf}
  ${If} ${FileExists} "$0"
    nsExec::ExecToLog '"$0" --uninstall'
  ${EndIf}

  nsExec::ExecToLog 'sc.exe stop nimbus-split-tunnel'
  nsExec::ExecToLog 'sc.exe delete nimbus-split-tunnel'
  Delete "$SYSDIR\drivers\nimbus-split-tunnel.sys"

  Pop $0
!macroend
