# #1277: compile/run the real NSIS identity hook against isolated registry keys, no app install.
param([string]$MakeNsis = (Join-Path $env:LOCALAPPDATA 'tauri/NSIS/makensis.exe'))
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$scratch = Join-Path $repoRoot ".dex/tmp/identity-installer-$PID"
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$registryPrefix = "Software\DexIdentityInstallerTest-$PID"
$hook = Get-Content -LiteralPath (Join-Path $repoRoot 'src-tauri/windows/identity-installer.nsh') -Raw
$hook = $hook.Replace('Software\arda\', "$registryPrefix\arda\")
[IO.File]::WriteAllText((Join-Path $scratch 'identity-hook.nsh'), $hook)
$fixture = @'
Unicode true
; The fixture only uses isolated HKCU keys; never require elevation or a UAC prompt.
RequestExecutionLevel user
SilentInstall silent
OutFile "identity-installer-test.exe"
!include "LogicLib.nsh"
!include "x64.nsh"
!macro SetContext
!macroend
!include "identity-hook.nsh"
!define INSTALLMODE "currentUser"
!define ARCH "x64"
!define PRODUCTNAME "Dex"
!define REGBASE "@@REGISTRY@@"
!define MANUPRODUCTKEY "${REGBASE}\hakunokina\Dex"
!define UNINSTKEY "${REGBASE}\Uninstall\Dex"

Function .onInit
  SetRegView 64
  WriteRegStr HKCU "${REGBASE}\arda\Dex" "" "$EXEDIR\old-install"
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" "$EXEDIR\old-install\uninstall.exe"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "arda"
  !insertmacro SetContext
  ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
  StrCmp $4 "$EXEDIR\old-install" +2
    Goto failed
  ReadRegStr $4 SHCTX "${REGBASE}\arda\Dex" ""
  StrCmp $4 "$EXEDIR\old-install" +2
    Goto failed
  ; A second run cannot change the copied value.
  !insertmacro SetContext
  ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
  StrCmp $4 "$EXEDIR\old-install" +2
    Goto failed
  ; An existing new publisher install location is authoritative.
  WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" "$EXEDIR\new-custom-install"
  !insertmacro SetContext
  ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
  StrCmp $4 "$EXEDIR\new-custom-install" +2
    Goto failed
  ; Restore the first migration and exercise the upstream restore/registration operations.
  WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" "$EXEDIR\old-install"
  ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
  StrCpy $INSTDIR $4
  Return
  failed:
    DeleteRegKey HKCU "${REGBASE}"
    SetErrorLevel 1
    Abort
FunctionEnd

Section
  StrCmp $INSTDIR "$EXEDIR\old-install" +2
    Goto failed
  WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "hakunokina"
  WriteRegStr SHCTX "${UNINSTKEY}" "InstallLocation" $INSTDIR
  WriteRegStr SHCTX "${UNINSTKEY}" "UninstallString" "$INSTDIR\uninstall.exe"
  EnumRegKey $R1 HKCU "${REGBASE}\Uninstall" 0
  StrCmp $R1 "Dex" +2
    Goto failed
  EnumRegKey $R1 HKCU "${REGBASE}\Uninstall" 1
  StrCmp $R1 "" +2
    Goto failed
  ReadRegStr $R1 SHCTX "${UNINSTKEY}" "Publisher"
  StrCmp $R1 "hakunokina" +2
    Goto failed
  ReadRegStr $R1 SHCTX "${UNINSTKEY}" "InstallLocation"
  StrCmp $R1 "$EXEDIR\old-install" +2
    Goto failed
  DeleteRegKey HKCU "${REGBASE}"
  SetErrorLevel 0
  Goto done
  failed:
    DeleteRegKey HKCU "${REGBASE}"
    SetErrorLevel 2
  done:
SectionEnd
'@
[IO.File]::WriteAllText((Join-Path $scratch 'identity-installer-test.nsi'), $fixture.Replace('@@REGISTRY@@', $registryPrefix))
& $MakeNsis /V2 (Join-Path $scratch 'identity-installer-test.nsi')
if ($LASTEXITCODE -ne 0) { throw "NSIS fixture compile failed: $LASTEXITCODE" }
$testProcess = Start-Process -FilePath (Join-Path $scratch 'identity-installer-test.exe') -WindowStyle Hidden -Wait -PassThru
if ($testProcess.ExitCode -ne 0) { throw "NSIS identity regression: exit=$($testProcess.ExitCode)" }
Write-Output 'NSIS: 5 checks passed (legacy path, source retained, idempotence, existing new path, one upgraded product registration).'
