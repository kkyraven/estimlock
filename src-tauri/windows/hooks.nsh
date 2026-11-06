; The "Start with Windows" switch writes these values, and the default uninstaller leaves them,
; which would point Windows at a missing exe. Kept on an in-place update.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "estimlock"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "estimlock"
  ${EndIf}
!macroend
