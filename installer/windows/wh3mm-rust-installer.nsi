!ifndef APP_VERSION
!define APP_VERSION "0.1.0-alpha"
!endif

!ifndef PAYLOAD_DIR
!error "PAYLOAD_DIR must point to the staged Windows payload."
!endif

!ifndef OUT_FILE
!define OUT_FILE "WH3-Mod-Manager-Rust-Installer.exe"
!endif

Unicode true
Name "WH3 Mod Manager Rust"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\WH3 Mod Manager Rust"
InstallDirRegKey HKCU "Software\WH3 Mod Manager Rust" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "WH3 Mod Manager Rust ${APP_VERSION}"

!include "MUI2.nsh"

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "WH3 Mod Manager Rust" SecMain
  SectionIn RO

  SetOutPath "$INSTDIR"
  RMDir /r "$INSTDIR\helpers"
  RMDir /r "$INSTDIR\schema"
  File /r "${PAYLOAD_DIR}\*.*"

  WriteRegStr HKCU "Software\WH3 Mod Manager Rust" "InstallDir" "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\WH3 Mod Manager Rust"
  CreateShortcut "$SMPROGRAMS\WH3 Mod Manager Rust\WH3 Mod Manager Rust.lnk" "$INSTDIR\wh3mm-dioxus.exe"

  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust" "DisplayName" "WH3 Mod Manager Rust"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust" "Publisher" "WH3 Mod Manager"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust" "NoRepair" 1
SectionEnd

Section /o "Desktop shortcut" SecDesktopShortcut
  CreateShortcut "$DESKTOP\WH3 Mod Manager Rust.lnk" "$INSTDIR\wh3mm-dioxus.exe"
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\WH3 Mod Manager Rust.lnk"
  Delete "$SMPROGRAMS\WH3 Mod Manager Rust\WH3 Mod Manager Rust.lnk"
  RMDir "$SMPROGRAMS\WH3 Mod Manager Rust"

  Delete "$INSTDIR\Uninstall.exe"
  Delete "$INSTDIR\wh3mm-dioxus.exe"
  RMDir /r "$INSTDIR\helpers"
  RMDir /r "$INSTDIR\schema"
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WH3 Mod Manager Rust"
  DeleteRegKey HKCU "Software\WH3 Mod Manager Rust"
SectionEnd
