; Compiles a real checkbox page and exercises the actual NSIS registry macros.
; /S runs regression checks only. Registry writes are confined to this test key.
Unicode true
RequestExecutionLevel user
!include MUI2.nsh
!include LogicLib.nsh
!define DX_TEST_ROOT "Software\DiscXplorerInstallerRegression"
!define DX_CLASSES "${DX_TEST_ROOT}\Classes"
!define PRODUCTNAME "Disc Xplorer"
!define MAINBINARYNAME "tauri-app"
!define MANUPRODUCTKEY "${DX_TEST_ROOT}\App"
!define UNINSTKEY "${DX_TEST_ROOT}\Uninstall"
!define /ifndef DX_TEST_OUTPUT "installer-associations.exe"
!define /ifndef DX_TEST_HOOKS "../src-tauri/installer-hooks.nsh"
!include "${DX_TEST_HOOKS}"
; No shell notification is needed for the isolated namespace.
!macro UPDATEFILEASSOC
!macroend
Name "Disc Xplorer file type tests"
OutFile "${DX_TEST_OUTPUT}"
Var PassiveMode
Var UpdateMode
!insertmacro DX_ASSOCIATION_FUNCTIONS
Page custom DX_AssociationPage
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_LANGUAGE English

!macro EXPECT ACTUAL EXPECTED DESCRIPTION
  ${If} `${ACTUAL}` != `${EXPECTED}`
    FileWrite $9 "FAIL: ${DESCRIPTION}$\r$\n"
    FileClose $9
    DeleteRegKey HKCU "${DX_TEST_ROOT}"
    SetErrorLevel 1
    Quit
  ${EndIf}
!macroend

!macro REG_EXPECT KEY VALUE EXPECTED DESCRIPTION
  ReadRegStr $2 SHCTX "${KEY}" "${VALUE}"
  !insertmacro EXPECT "$2" "${EXPECTED}" "${DESCRIPTION}"
!macroend

!macro NONE EXT LABEL X Y
  StrCpy $DX_State_${EXT} ${BST_UNCHECKED}
!macroend
!macro ALL_CHECKED EXT LABEL X Y
  !insertmacro EXPECT $DX_State_${EXT} ${BST_CHECKED} "Fresh default ${EXT}"
!macroend

Function .onInit
  SetShellVarContext current
  StrCpy $INSTDIR "$TEMP\Disc Xplorer Test"
  StrCpy $PassiveMode 0
  StrCpy $UpdateMode 0
  DeleteRegKey HKCU "${DX_TEST_ROOT}"
FunctionEnd

Section
  FileOpen $9 "$EXEDIR\installer-associations.log" w
  Call DX_InitAssociations
  !insertmacro DX_TYPES ALL_CHECKED
  !insertmacro DX_TYPES NONE
  StrCpy $DX_State_bin ${BST_CHECKED}
  StrCpy $DX_State_img ${BST_CHECKED}
  WriteRegStr SHCTX "${DX_CLASSES}\.bin" "" "Previous.Bin"
  WriteRegStr SHCTX "${DX_CLASSES}\.iso" "" "Previous.Iso"
  WriteRegStr SHCTX "${DX_CLASSES}\Previous.Iso\DefaultIcon" "" "previous.ico"
  !insertmacro DX_INSTALL_ASSOCIATIONS
  !insertmacro REG_EXPECT "${DX_CLASSES}\.bin" "" "DiscXplorer.bin" "Selected BIN"
  !insertmacro REG_EXPECT "${DX_CLASSES}\.img" "" "DiscXplorer.img" "Independent IMG"
  !insertmacro REG_EXPECT "${DX_CLASSES}\.iso" "" "Previous.Iso" "Unselected ISO untouched"
  !insertmacro REG_EXPECT "${DX_CLASSES}\Previous.Iso\DefaultIcon" "" "previous.ico" "Unselected icon untouched"
  !insertmacro REG_EXPECT "${DX_CLASSES}\DiscXplorer.bin\shell\open\command" "" '$\"$INSTDIR\tauri-app.exe$\" $\"%1$\"' "Quoted launch command"

  ; Reinstall preserves the backup instead of replacing it with our own ProgID.
  !insertmacro DX_INSTALL_ASSOCIATIONS
  !insertmacro REG_EXPECT "${DX_CLASSES}\.bin" "DiscXplorer.bin_backup" "Previous.Bin" "Reinstall backup"
  StrCpy $DX_AssociationsReady 0
  StrCpy $DX_State_iso 1
  Call DX_InitAssociations
  !insertmacro EXPECT $DX_State_iso 0 "Saved unchecked ISO"
  !insertmacro EXPECT $DX_State_bin 1 "Saved checked BIN"

  ; Removing one association must preserve another selected type's class/icon.
  StrCpy $DX_State_bin ${BST_UNCHECKED}
  !insertmacro DX_INSTALL_ASSOCIATIONS
  !insertmacro REG_EXPECT "${DX_CLASSES}\.bin" "" "Previous.Bin" "Deselect restores previous BIN"
  !insertmacro REG_EXPECT "${DX_CLASSES}\.img" "" "DiscXplorer.img" "IMG still selected"
  !insertmacro REG_EXPECT "${DX_CLASSES}\DiscXplorer.img\DefaultIcon" "" '$\"$INSTDIR\tauri-app.exe$\",0' "IMG icon still exists"
  WriteRegStr SHCTX "${DX_CLASSES}\.img" "" "Other.NewChoice"
  !insertmacro DX_UNINSTALL_ASSOCIATIONS
  !insertmacro REG_EXPECT "${DX_CLASSES}\.img" "" "Other.NewChoice" "Uninstall preserves newer app choice"
  !insertmacro REG_EXPECT "${DX_CLASSES}\.iso" "" "Previous.Iso" "Uninstall does not claim unselected ISO"

  ; Migrate a pre-checkbox install without first running its uninstaller.
  DeleteRegKey HKCU "${DX_TEST_ROOT}"
  WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" "$INSTDIR"
  WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "tauri-app.exe"
  WriteRegStr SHCTX "${DX_CLASSES}\Disc Image\shell\open\command" "" '$INSTDIR\tauri-app.exe $\"%1$\"'
  WriteRegStr SHCTX "${DX_CLASSES}\.iso" "" "Disc Image"
  WriteRegStr SHCTX "${DX_CLASSES}\.iso" "Disc Image_backup" "Previous.Iso"
  WriteRegStr SHCTX "${DX_CLASSES}\.bin" "" "Disc Image"
  WriteRegStr SHCTX "${DX_CLASSES}\.bin" "Disc Image_backup" "Previous.Bin"
  StrCpy $DX_AssociationsReady 0
  Call DX_InitAssociations
  !insertmacro EXPECT $DX_State_iso 1 "Legacy selected ISO"
  !insertmacro EXPECT $DX_State_img 0 "Legacy unselected IMG"
  StrCpy $DX_State_bin 0
  !insertmacro DX_INSTALL_ASSOCIATIONS
  !insertmacro REG_EXPECT "${DX_CLASSES}\.bin" "" "Previous.Bin" "Legacy deselection"
  !insertmacro REG_EXPECT "${DX_CLASSES}\.iso" "DiscXplorer.iso_backup" "Previous.Iso" "Legacy backup migration"
  !insertmacro DX_UNINSTALL_ASSOCIATIONS
  !insertmacro REG_EXPECT "${DX_CLASSES}\.iso" "" "Previous.Iso" "Migrated uninstall restores ISO"

  ; A class with the same old generic name can belong to another program.
  DeleteRegKey HKCU "${DX_TEST_ROOT}"
  WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" "$INSTDIR"
  WriteRegStr SHCTX "${DX_CLASSES}\Disc Image\shell\open\command" "" "other-app.exe"
  WriteRegStr SHCTX "${DX_CLASSES}\.iso" "" "Disc Image"
  StrCpy $DX_AssociationsReady 0
  Call DX_InitAssociations
  !insertmacro EXPECT $DX_State_iso 0 "Foreign shared class not selected"
  !insertmacro DX_INSTALL_ASSOCIATIONS
  !insertmacro REG_EXPECT "${DX_CLASSES}\Disc Image\shell\open\command" "" "other-app.exe" "Foreign shared class preserved"

  DeleteRegKey HKCU "${DX_TEST_ROOT}"
  FileWrite $9 "PASS: file association registration, repair, migration, preferences, and removal.$\r$\n"
  FileClose $9
  SetErrorLevel 0
SectionEnd
