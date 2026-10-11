; Publisher changes, but the NSIS product remains Dex. Preserve the previous install directory
; before Tauri's restore/reinstall code reads the new publisher key (including silent installs).
; SetContext matches Tauri CLI 2.11.5 utils.nsh; only installer .onInit adds the copy below.
!macroundef SetContext
!macro SetContext
  !if "${INSTALLMODE}" == "currentUser"
    SetShellVarContext current
  !else if "${INSTALLMODE}" == "perMachine"
    SetShellVarContext all
  !endif
  ${If} ${RunningX64}
    !if "${ARCH}" == "x64"
      SetRegView 64
    !else if "${ARCH}" == "arm64"
      SetRegView 64
    !else
      SetRegView 32
    !endif
  ${EndIf}
  !if "${__FUNCTION__}" == ".onInit"
    Push $R8
    Push $R9
    ReadRegStr $R8 SHCTX "${MANUPRODUCTKEY}" ""
    ReadRegStr $R9 SHCTX "${UNINSTKEY}" "UninstallString"
    ${If} $R8 == ""
    ${AndIf} $R9 != ""
      ReadRegStr $R8 SHCTX "Software\arda\${PRODUCTNAME}" ""
      ${If} $R8 != ""
        WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" $R8
      ${EndIf}
    ${EndIf}
    Pop $R9
    Pop $R8
    ClearErrors
  !endif
!macroend
