; Nimbus installer hooks (Windows).
;
; Two pieces get registered with the Service Control Manager:
;
;   nimbus-split-tunnel  the signed WFP driver (kernel service)
;   nimbus-split          the LocalSystem agent that owns it and serves the UI
;
; Both are installed by `nimbus-split-win.exe`, not by this script. That is not
; only about quoting: NSIS is a 32-bit process, so under WOW64 file-system
; redirection `$SYSDIR` resolves to `SysWOW64`, and a driver copied there is one
; the kernel will not load. The first version of this file did exactly that and
; left a service pointing at a file that did not exist. The agent binary is
; 64-bit, so `%SystemRoot%\System32` means what it says.
;
; Everything is best-effort: failing to register the services must not abort the
; install. Without them Nimbus still runs, it just reports that app split
; tunneling needs its service.

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1

  ; The service binary, wherever the bundler put the resources.
  StrCpy $0 "$INSTDIR\nimbus-split-win.exe"
  ${IfNot} ${FileExists} "$0"
    StrCpy $0 "$INSTDIR\resources\nimbus-split-win.exe"
  ${EndIf}

  ${If} ${FileExists} "$0"
    nsExec::ExecToLog '"$0" --install'

    StrCpy $1 "$INSTDIR\mullvad-split-tunnel.sys"
    ${IfNot} ${FileExists} "$1"
      StrCpy $1 "$INSTDIR\resources\mullvad-split-tunnel.sys"
    ${EndIf}
    ${If} ${FileExists} "$1"
      nsExec::ExecToLog '"$0" --install-driver "$1"'
    ${EndIf}
  ${EndIf}

  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Push $0

  ; Stop the agent first: it holds the driver's exclusive control device, so
  ; stopping the driver while the agent runs would leave it wedged.
  StrCpy $0 "$INSTDIR\nimbus-split-win.exe"
  ${IfNot} ${FileExists} "$0"
    StrCpy $0 "$INSTDIR\resources\nimbus-split-win.exe"
  ${EndIf}
  ${If} ${FileExists} "$0"
    nsExec::ExecToLog '"$0" --uninstall'
    nsExec::ExecToLog '"$0" --uninstall-driver'
  ${EndIf}

  Pop $0
!macroend
