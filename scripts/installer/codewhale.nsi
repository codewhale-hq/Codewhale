; codewhale.nsi — NSIS installer for CodeWhale (Windows)
;
; Requirements (see https://github.com/codewhale-hq/CodeWhale/issues/1983):
;   - Install codewhale.exe and codew.exe side-by-side (single binary, no codewhale-tui.exe)
;   - Default to %LOCALAPPDATA%\Programs\CodeWhale\bin
;   - Add install dir to current-user PATH
;   - Uninstaller removes the PATH entry
;   - Install codewhale.bat and a current-user Start Menu shortcut (#1854)
;   - Uninstaller removes the launcher and shortcut
;
; Usage:
;   1. Place the binaries next to this script (codewhale.bat is already here):
;        codewhale.exe
;        codew.exe
;   2. Build:
;        makensis /DVERSION=1.2.3 codewhale.nsi
;   3. Output: CodeWhaleSetup.exe (in current directory)

;--------------------------------
; Includes
;--------------------------------
!include "MUI2.nsh"
!include "FileFunc.nsh"

;--------------------------------
; General
;--------------------------------
!ifndef VERSION
  !define VERSION "0.0.0"
!endif

!define PRODUCT_NAME "CodeWhale"
!define PRODUCT_PUBLISHER "Hmbown"
!define PRODUCT_WEB_SITE "https://github.com/codewhale-hq/CodeWhale"

Name "${PRODUCT_NAME} ${VERSION}"
OutFile "CodeWhaleSetup.exe"
InstallDir "$LOCALAPPDATA\Programs\CodeWhale"
RequestExecutionLevel user
BrandingText "${PRODUCT_NAME} Installer"

;--------------------------------
; Interface Settings
;--------------------------------
!define MUI_ABORTWARNING
!define MUI_ICON "${NSISDIR}\Contrib\Graphics\Icons\modern-install.ico"
!define MUI_UNICON "${NSISDIR}\Contrib\Graphics\Icons\modern-uninstall.ico"

;--------------------------------
; Pages
;--------------------------------
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!ifdef COMPILED_HOST
!insertmacro MUI_PAGE_COMPONENTS
!endif
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

;--------------------------------
; Languages
;--------------------------------
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

;--------------------------------
; Installer Sections
;--------------------------------
Section "Install" SecInstall
  SetOutPath "$INSTDIR"
  File "update-user-path.ps1"

  SetOutPath "$INSTDIR\bin"

  ; Copy binaries (single binary) and the Windows Terminal launcher (#1854)
  File "codewhale.exe"
  File "codew.exe"
  File "codewhale.bat"

  ; Write uninstaller
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk" "$INSTDIR\bin\codewhale.bat" "" "$INSTDIR\bin\codewhale.exe" 0

  ; NSIS strings default to 1024 characters. ReadRegStr returns an empty string
  ; when a registry value exceeds that limit, which used to make a long user
  ; PATH look absent and overwrite it. Use PowerShell/.NET registry APIs so the
  ; complete raw value and its REG_SZ/REG_EXPAND_SZ kind are preserved.
  Call AddToUserPath

  ; Store install directory for uninstaller
  WriteRegStr HKCU "Software\${PRODUCT_NAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "QuietUninstallString" "$\"$INSTDIR\Uninstall.exe$\" /S"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "URLInfoAbout" "${PRODUCT_WEB_SITE}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "DisplayVersion" "${VERSION}"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "NoRepair" 1

  ; Calculate and store installed size
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "EstimatedSize" "$0"
SectionEnd

Function AddToUserPath
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\update-user-path.ps1" -Operation Add -Entry "$INSTDIR\bin"' $0
  IntCmp $0 0 add_path_done
    DetailPrint "Failed to add CodeWhale to the current-user PATH (exit code $0)."
    IfSilent +2
      MessageBox MB_ICONSTOP|MB_OK "CodeWhale could not safely update your user PATH. Installation has stopped without replacing the existing PATH."
    SetErrorLevel 1
    Abort

  add_path_done:
    SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
FunctionEnd

;--------------------------------
; Uninstaller Section
;--------------------------------
; Only a reviewed release build defines COMPILED_HOST. The component is
; explicitly opt-in and never changes [extension_host] runtime = "node".
!ifdef COMPILED_HOST
Section /o "Compiled extension host (Bun, opt-in)" SecCompiledHost
  SetOutPath "$INSTDIR\bin"
  File "codewhale-extension-host.exe"
  File "codewhale-extension-host.LICENSES.txt"
  File "codewhale-extension-host.relink-source.tar.gz"
  File "codewhale-extension-host.release.json"
  WriteRegDWORD HKCU "Software\${PRODUCT_NAME}" "CompiledHostInstalled" 1
SectionEnd
!endif

Section "Uninstall"
  ; Remove only CodeWhale's exact entry before deleting the helper. The helper
  ; handles PATH values longer than NSIS_MAX_STRLEN without truncation.
  Call un.RemoveFromUserPath

  ; Remove binaries, launcher, and Start Menu shortcut (single binary)
  Delete "$INSTDIR\bin\codewhale.exe"
  Delete "$INSTDIR\bin\codew.exe"
  Delete "$INSTDIR\bin\codewhale.bat"
  ReadRegDWORD $R0 HKCU "Software\${PRODUCT_NAME}" "CompiledHostInstalled"
  ${If} $R0 == 1
    Delete "$INSTDIR\bin\codewhale-extension-host.exe"
    Delete "$INSTDIR\bin\codewhale-extension-host.LICENSES.txt"
    Delete "$INSTDIR\bin\codewhale-extension-host.relink-source.tar.gz"
    Delete "$INSTDIR\bin\codewhale-extension-host.release.json"
  ${EndIf}
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk"
  RMDir "$SMPROGRAMS\${PRODUCT_NAME}"
  Delete "$INSTDIR\update-user-path.ps1"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR\bin"
  RMDir "$INSTDIR"

  ; Remove registry keys
  DeleteRegValue HKCU "Software\${PRODUCT_NAME}" "InstallDir"
  DeleteRegValue HKCU "Software\${PRODUCT_NAME}" "CompiledHostInstalled"
  DeleteRegKey /ifempty HKCU "Software\${PRODUCT_NAME}"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
SectionEnd

Function un.RemoveFromUserPath
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\update-user-path.ps1" -Operation Remove -Entry "$INSTDIR\bin"' $0
  IntCmp $0 0 remove_path_done
    DetailPrint "Failed to remove CodeWhale from the current-user PATH (exit code $0)."
    IfSilent +2
      MessageBox MB_ICONSTOP|MB_OK "CodeWhale could not safely update your user PATH. Uninstallation has stopped without replacing the existing PATH."
    SetErrorLevel 1
    Abort

  remove_path_done:
    SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
FunctionEnd
