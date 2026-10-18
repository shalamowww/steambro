; The steambro installer, built by release.sh with NSIS.
; A per-user install: no administrator rights, files in %LOCALAPPDATA%\Programs\steambro, a Start
; menu entry, and an entry in Windows' list of installed apps with an uninstaller.
; release.sh passes VERSION, EXE, LOADER, LICENSE, ICON, and OUT with /D.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!define KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\steambro"

Name "steambro ${VERSION}"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\steambro"
InstallDirRegKey HKCU "${KEY}" "InstallLocation"
BrandingText "steambro ${VERSION}"

!include "MUI2.nsh"
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\steambro.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start steambro"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "steambro"
VIAddVersionKey "FileDescription" "steambro installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright (C) 2026 shalamowww. GNU GPL version 3."

Section "steambro"
  ; An update replaces the exe, so a running steambro has to end first. Game launches end too.
  nsExec::Exec 'taskkill /IM steambro.exe /F'
  Sleep 500
  SetOutPath "$INSTDIR"
  File "/oname=steambro.exe" "${EXE}"
  File "/oname=WebView2Loader.dll" "${LOADER}"
  File "/oname=LICENSE.txt" "${LICENSE}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\steambro.lnk" "$INSTDIR\steambro.exe"
  WriteRegStr HKCU "${KEY}" "DisplayName" "steambro"
  WriteRegStr HKCU "${KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${KEY}" "Publisher" "shalamowww"
  WriteRegStr HKCU "${KEY}" "URLInfoAbout" "https://github.com/shalamowww/steambro"
  WriteRegStr HKCU "${KEY}" "DisplayIcon" "$INSTDIR\steambro.exe"
  WriteRegStr HKCU "${KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${KEY}" "NoRepair" 1
SectionEnd

; Leaves %LOCALAPPDATA%\steambro (the state, the settings, the art cache, the log), Steam's
; shortcuts, and the ViGEmBus driver, which has its own uninstaller.
Section "Uninstall"
  nsExec::Exec 'taskkill /IM steambro.exe /F'
  Sleep 500
  Delete "$INSTDIR\steambro.exe"
  Delete "$INSTDIR\WebView2Loader.dll"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\steambro.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "steambro"
  DeleteRegKey HKCU "${KEY}"
SectionEnd
