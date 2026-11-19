; Kurnace Windows installer (NSIS). Build with:
;   makensis -DSTAGE=... -DOUTFILE=... -DKURN_VERSION=0.0.1 installer/windows.nsi
Unicode true
!include "MUI2.nsh"

!ifndef STAGE
  !error "STAGE is required"
!endif
!ifndef OUTFILE
  !error "OUTFILE is required"
!endif
!ifndef KURN_VERSION
  !define KURN_VERSION "0.0.1"
!endif

Name "Kurnace"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Kurnace"
InstallDirRegKey HKCU "Software\Kurnace" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\Kurnace.exe"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Kurnace" SecMain
  SetRegView 64
  SetOutPath "$INSTDIR"
  File /r "${STAGE}/*.*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Kurnace" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kurnace" "DisplayName" "Kurnace"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kurnace" "DisplayVersion" "${KURN_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kurnace" "Publisher" "Gubran Dormoush"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kurnace" "UninstallString" "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Kurnace"
  CreateShortcut "$SMPROGRAMS\Kurnace\Kurnace.lnk" "$INSTDIR\Kurnace.exe"
  CreateShortcut "$SMPROGRAMS\Kurnace\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  CreateShortcut "$DESKTOP\Kurnace.lnk" "$INSTDIR\Kurnace.exe"
SectionEnd

Section "Uninstall"
  SetRegView 64
  Delete "$DESKTOP\Kurnace.lnk"
  Delete "$SMPROGRAMS\Kurnace\Kurnace.lnk"
  Delete "$SMPROGRAMS\Kurnace\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Kurnace"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kurnace"
  DeleteRegKey HKCU "Software\Kurnace"
  RMDir /r "$INSTDIR"
SectionEnd
