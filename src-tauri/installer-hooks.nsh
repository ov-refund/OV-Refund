; OV Refund uninstall hooks.
; Updates ($UpdateMode = 1) run the old uninstaller too: then nothing is removed, so automatic
; checks, the login and all data simply continue in the new version.
; A real uninstall always removes the scheduled task, the stored NS password and the saved NS
; session (they must never outlive the app). If the user also ticks "Delete the application
; data": the data folder (%LOCALAPPDATA%\OVRefund) and all OV Refund credentials (IBAN, card
; numbers, mail app password) go too. The WebView2 profile is always removed.
;
; Order matters: this hook runs BEFORE the uninstaller's own "is the app running?" check, so a
; running OV Refund (window or scheduled check) could otherwise write its session or data back
; after the wipe. 1) remove the scheduled task (no new check starts), 2) stop every running
; ov-refund.exe of this user (per-user install), 3) forget-login / forget-all, which also take
; the run lock and stop a process that still holds it after ~60 s.
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    ; The app removes this user's task itself, with its own name rules (per-user name with a
    ; hash of DOMAIN\USER, plus the older 0.1.2 per-user and pre-0.1.2 shared names, only when
    ; this user created them). The installer never builds task names itself (no stack use).
    ExecWait '"$INSTDIR\ov-refund.exe" remove-schedule'
    UserInfo::GetName
    Pop $0
    nsExec::Exec 'taskkill /F /T /IM ov-refund.exe /FI "USERNAME eq $0"'
    Sleep 1000
    ExecWait '"$INSTDIR\ov-refund.exe" forget-login'
    ${If} $DeleteAppDataCheckboxState = 1
      ExecWait '"$INSTDIR\ov-refund.exe" forget-all'
      RmDir /r "$LOCALAPPDATA\OVRefund"
    ${EndIf}
    ; Identifier: nl.ovrefund.app. Always remove NS cookies, storage and history.
    RmDir /r "$LOCALAPPDATA\nl.ovrefund.app\EBWebView"
  ${EndIf}
!macroend
