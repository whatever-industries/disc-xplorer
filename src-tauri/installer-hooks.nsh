; Individual file associations for the custom Tauri NSIS template.
!include nsDialogs.nsh
!include LogicLib.nsh

; Overridden only by the isolated Windows registry regression harness.
!define /ifndef DX_CLASSES "Software\Classes"

; One catalog drives the UI, persisted choices, registration and removal.
; Columns: disc images, compressed/console images, other/raw images.
!macro DX_TYPES CALLBACK
  !insertmacro ${CALLBACK} iso   "ISO (.iso)"       0u   37u
  !insertmacro ${CALLBACK} bin   "BIN (.bin)"       0u   47u
  !insertmacro ${CALLBACK} img   "IMG (.img)"       0u   57u
  !insertmacro ${CALLBACK} cue   "CUE (.cue)"       0u   67u
  !insertmacro ${CALLBACK} mds   "MDS (.mds)"       0u   77u
  !insertmacro ${CALLBACK} mdx   "MDX (.mdx)"       0u   87u
  !insertmacro ${CALLBACK} nrg   "NRG (.nrg)"       0u   97u
  !insertmacro ${CALLBACK} ccd   "CCD (.ccd)"       0u  107u
  !insertmacro ${CALLBACK} cdi   "CDI (.cdi)"       0u  117u
  !insertmacro ${CALLBACK} gdi   "GDI (.gdi)"       0u  127u
  !insertmacro ${CALLBACK} chd   "CHD (.chd)"     102u   37u
  !insertmacro ${CALLBACK} cso   "CSO (.cso)"     102u   47u
  !insertmacro ${CALLBACK} ciso  "CISO (.ciso)"   102u   57u
  !insertmacro ${CALLBACK} zso   "ZSO (.zso)"     102u   67u
  !insertmacro ${CALLBACK} ecm   "ECM (.ecm)"     102u   77u
  !insertmacro ${CALLBACK} uif   "UIF (.uif)"     102u   87u
  !insertmacro ${CALLBACK} wbfs  "WBFS (.wbfs)"   102u   97u
  !insertmacro ${CALLBACK} wux   "WUX (.wux)"     102u  107u
  !insertmacro ${CALLBACK} wud   "WUD (.wud)"     102u  117u
  !insertmacro ${CALLBACK} fatx  "FATX (.fatx)"   102u  127u
  !insertmacro ${CALLBACK} b5t   "B5T (.b5t)"     204u   37u
  !insertmacro ${CALLBACK} b6t   "B6T (.b6t)"     204u   47u
  !insertmacro ${CALLBACK} bwt   "BWT (.bwt)"     204u   57u
  !insertmacro ${CALLBACK} c2d   "C2D (.c2d)"     204u   67u
  !insertmacro ${CALLBACK} pdi   "PDI (.pdi)"     204u   77u
  !insertmacro ${CALLBACK} daa   "DAA (.daa)"     204u   87u
  !insertmacro ${CALLBACK} cif   "CIF (.cif)"     204u   97u
  !insertmacro ${CALLBACK} scram "SCRAM (.scram)" 204u  107u
  !insertmacro ${CALLBACK} sdram "SDRAM (.sdram)" 204u  117u
  !insertmacro ${CALLBACK} sbram "SBRAM (.sbram)" 204u  127u
!macroend

!macro DX_DECLARE EXT LABEL X Y
  Var DX_Check_${EXT}
  Var DX_State_${EXT}
!macroend
!insertmacro DX_TYPES DX_DECLARE
Var DX_AssociationsReady
Var DX_Dialog
Var DX_LegacyOwned
Var DX_PreviousInstall

!macro DX_LOAD EXT LABEL X Y
  ClearErrors
  ReadRegDWORD $DX_State_${EXT} SHCTX "${MANUPRODUCTKEY}\FileAssociations" "${EXT}"
  ${If} ${Errors}
    ; Preserve an older install's current associations; new installs keep the
    ; previous installer's all-selected default. A saved zero is never lost.
    StrCpy $DX_State_${EXT} ${BST_CHECKED}
    ${If} $DX_PreviousInstall != ""
      ReadRegStr $0 SHCTX "${DX_CLASSES}\.${EXT}" ""
      StrCpy $DX_State_${EXT} ${BST_UNCHECKED}
      ${If} $0 == "DiscXplorer.${EXT}"
        StrCpy $DX_State_${EXT} ${BST_CHECKED}
      ${ElseIf} $0 == "Disc Image"
      ${AndIf} $DX_LegacyOwned == 1
        StrCpy $DX_State_${EXT} ${BST_CHECKED}
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro DX_CHECKBOX EXT LABEL X Y
  ${NSD_CreateCheckbox} ${X} ${Y} 96u 10u "${LABEL}"
  Pop $DX_Check_${EXT}
  ${NSD_SetState} $DX_Check_${EXT} $DX_State_${EXT}
  ${NSD_OnClick} $DX_Check_${EXT} DX_RememberChoices
!macroend

!macro DX_REMEMBER EXT LABEL X Y
  ${NSD_GetState} $DX_Check_${EXT} $DX_State_${EXT}
!macroend

!macro DX_SET_ALL EXT LABEL X Y
  StrCpy $DX_State_${EXT} $0
  ${NSD_SetState} $DX_Check_${EXT} $0
!macroend

; Expanded after Tauri defines PRODUCTNAME, MAINBINARYNAME and registry keys.
!macro DX_ASSOCIATION_FUNCTIONS
Function DX_InitAssociations
  ${If} $DX_AssociationsReady == 1
    Return
  ${EndIf}
  Push $0
  Push $1
  ReadRegStr $DX_PreviousInstall SHCTX "${MANUPRODUCTKEY}" ""
  ReadRegStr $0 SHCTX "${DX_CLASSES}\Disc Image\shell\open\command" ""
  StrCpy $DX_LegacyOwned 0
  ${If} $DX_PreviousInstall != ""
    ReadRegStr $1 SHCTX "${UNINSTKEY}" "MainBinaryName"
    ${If} $1 == ""
      StrCpy $1 "${MAINBINARYNAME}.exe"
    ${EndIf}
    ${If} $0 == '$DX_PreviousInstall\$1 $\"%1$\"'
    ${OrIf} $0 == '$\"$DX_PreviousInstall\$1$\" $\"%1$\"'
      StrCpy $DX_LegacyOwned 1
    ${EndIf}
  ${EndIf}
  !insertmacro DX_TYPES DX_LOAD
  StrCpy $DX_AssociationsReady 1
  Pop $1
  Pop $0
FunctionEnd

Function DX_AssociationPage
  ${If} $PassiveMode == 1
  ${OrIf} $UpdateMode == 1
    Abort
  ${EndIf}
  Call DX_InitAssociations
  !insertmacro MUI_HEADER_TEXT "File types" "Choose which files open with Disc Xplorer when double-clicked."
  nsDialogs::Create 1018
  Pop $DX_Dialog
  ${If} $DX_Dialog == error
    Abort
  ${EndIf}
  ${NSD_CreateButton} 0u 0u 54u 14u "Select all"
  Pop $0
  ${NSD_OnClick} $0 DX_SelectAll
  ${NSD_CreateButton} 60u 0u 54u 14u "Clear all"
  Pop $0
  ${NSD_OnClick} $0 DX_ClearAll
  ${NSD_CreateLabel} 122u 2u 178u 12u "Selected types also use the app's icon."
  Pop $0
  ${NSD_CreateLabel} 0u 23u 96u 12u "Disc images"
  Pop $0
  ${NSD_CreateLabel} 102u 23u 96u 12u "Compressed / console"
  Pop $0
  ${NSD_CreateLabel} 204u 23u 96u 12u "Other / raw"
  Pop $0
  !insertmacro DX_TYPES DX_CHECKBOX
  nsDialogs::Show
FunctionEnd

Function DX_RememberChoices
  Pop $0 ; clicked control; persist immediately so Back also preserves choices
  !insertmacro DX_TYPES DX_REMEMBER
FunctionEnd

Function DX_SelectAll
  Pop $0
  StrCpy $0 ${BST_CHECKED}
  !insertmacro DX_TYPES DX_SET_ALL
FunctionEnd

Function DX_ClearAll
  Pop $0
  StrCpy $0 ${BST_UNCHECKED}
  !insertmacro DX_TYPES DX_SET_ALL
FunctionEnd
!macroend

!macro DX_REMOVE EXT LABEL X Y
  ; Never overwrite a handler chosen in another app after our installation.
  ReadRegStr $0 SHCTX "${DX_CLASSES}\.${EXT}" ""
  ${If} $0 == "DiscXplorer.${EXT}"
    ReadRegStr $1 SHCTX "${DX_CLASSES}\.${EXT}" "DiscXplorer.${EXT}_backup"
    ${If} $1 == ""
    ${OrIf} $1 == "DiscXplorer.${EXT}"
      DeleteRegValue SHCTX "${DX_CLASSES}\.${EXT}" ""
    ${Else}
      WriteRegStr SHCTX "${DX_CLASSES}\.${EXT}" "" "$1"
    ${EndIf}
  ${EndIf}
  DeleteRegValue SHCTX "${DX_CLASSES}\.${EXT}" "DiscXplorer.${EXT}_backup"
  DeleteRegKey /ifempty SHCTX "${DX_CLASSES}\.${EXT}"
  DeleteRegKey SHCTX "${DX_CLASSES}\DiscXplorer.${EXT}"
!macroend

!macro DX_INSTALL EXT LABEL X Y
  ; Migrate the old shared class only if it belonged to this installation.
  ${If} $DX_LegacyOwned == 1
    ReadRegStr $0 SHCTX "${DX_CLASSES}\.${EXT}" ""
    ${If} $0 == "Disc Image"
      ReadRegStr $1 SHCTX "${DX_CLASSES}\.${EXT}" "Disc Image_backup"
      ${If} $1 == ""
      ${OrIf} $1 == "Disc Image"
        DeleteRegValue SHCTX "${DX_CLASSES}\.${EXT}" ""
      ${Else}
        WriteRegStr SHCTX "${DX_CLASSES}\.${EXT}" "" "$1"
      ${EndIf}
      DeleteRegValue SHCTX "${DX_CLASSES}\.${EXT}" "Disc Image_backup"
    ${EndIf}
  ${EndIf}
  ${If} $DX_State_${EXT} == ${BST_CHECKED}
    ReadRegStr $0 SHCTX "${DX_CLASSES}\.${EXT}" ""
    ; A reinstall must not replace the original backup with our own class.
    ${If} $0 != "DiscXplorer.${EXT}"
      WriteRegStr SHCTX "${DX_CLASSES}\.${EXT}" "DiscXplorer.${EXT}_backup" "$0"
    ${EndIf}
    WriteRegStr SHCTX "${DX_CLASSES}\.${EXT}" "" "DiscXplorer.${EXT}"
    WriteRegStr SHCTX "${DX_CLASSES}\DiscXplorer.${EXT}" "" "${LABEL} disc image"
    WriteRegStr SHCTX "${DX_CLASSES}\DiscXplorer.${EXT}\DefaultIcon" "" '$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0'
    WriteRegStr SHCTX "${DX_CLASSES}\DiscXplorer.${EXT}\shell" "" "open"
    WriteRegStr SHCTX "${DX_CLASSES}\DiscXplorer.${EXT}\shell\open" "" "Open with ${PRODUCTNAME}"
    WriteRegStr SHCTX "${DX_CLASSES}\DiscXplorer.${EXT}\shell\open\command" "" '$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\"'
  ${Else}
    !insertmacro DX_REMOVE ${EXT} "${LABEL}" ${X} ${Y}
  ${EndIf}
  WriteRegDWORD SHCTX "${MANUPRODUCTKEY}\FileAssociations" "${EXT}" $DX_State_${EXT}
!macroend

!macro DX_INSTALL_ASSOCIATIONS
  Call DX_InitAssociations
  !insertmacro DX_TYPES DX_INSTALL
  ${If} $DX_LegacyOwned == 1
    DeleteRegKey SHCTX "${DX_CLASSES}\Disc Image"
  ${EndIf}
  !insertmacro UPDATEFILEASSOC
!macroend

!macro DX_UNINSTALL_ASSOCIATIONS
  !insertmacro DX_TYPES DX_REMOVE
  !insertmacro UPDATEFILEASSOC
!macroend
