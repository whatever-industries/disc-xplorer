; Windows installer hooks.
;
; Tauri's NSIS template writes the file associations unconditionally, from its
; install section, before NSIS_HOOK_POSTINSTALL runs. There is no config flag to
; make them optional, and replacing the whole template to add a checkbox would
; mean vendoring ~1000 lines that change between Tauri releases. So the choice is
; offered here instead, and declining removes what was just written.
;
; Issue #13: the associations also repaint every disc image in Explorer with our
; icon, which is the part people notice and object to. That icon lives on the
; "Disc Image" file class, so removing the class removes the icon too.

!macro NSIS_HOOK_POSTINSTALL
  ; /SD IDYES keeps silent and automated installs behaving exactly as before.
  MessageBox MB_YESNO|MB_ICONQUESTION \
    "Open disc images with Disc Xplorer when you double-click them?$\r$\n$\r$\nThis also gives them its icon in Explorer." \
    /SD IDYES IDYES DiscXplorer_KeepAssoc

    ; APP_UNASSOCIATE is the same macro Tauri's own uninstaller uses: it restores
    ; whichever handler each extension had before, from the backup APP_ASSOCIATE
    ; wrote, then deletes our file class. Keep this list in step with
    ; bundle.fileAssociations in tauri.conf.json.
    !define DX_UNASSOC "!insertmacro APP_UNASSOCIATE"
    ${DX_UNASSOC} "iso"   "Disc Image"
    ${DX_UNASSOC} "bin"   "Disc Image"
    ${DX_UNASSOC} "img"   "Disc Image"
    ${DX_UNASSOC} "cue"   "Disc Image"
    ${DX_UNASSOC} "chd"   "Disc Image"
    ${DX_UNASSOC} "mds"   "Disc Image"
    ${DX_UNASSOC} "mdx"   "Disc Image"
    ${DX_UNASSOC} "nrg"   "Disc Image"
    ${DX_UNASSOC} "ccd"   "Disc Image"
    ${DX_UNASSOC} "cdi"   "Disc Image"
    ${DX_UNASSOC} "gdi"   "Disc Image"
    ${DX_UNASSOC} "b5t"   "Disc Image"
    ${DX_UNASSOC} "b6t"   "Disc Image"
    ${DX_UNASSOC} "bwt"   "Disc Image"
    ${DX_UNASSOC} "c2d"   "Disc Image"
    ${DX_UNASSOC} "pdi"   "Disc Image"
    ${DX_UNASSOC} "daa"   "Disc Image"
    ${DX_UNASSOC} "cso"   "Disc Image"
    ${DX_UNASSOC} "ciso"  "Disc Image"
    ${DX_UNASSOC} "zso"   "Disc Image"
    ${DX_UNASSOC} "ecm"   "Disc Image"
    ${DX_UNASSOC} "uif"   "Disc Image"
    ${DX_UNASSOC} "cif"   "Disc Image"
    ${DX_UNASSOC} "wbfs"  "Disc Image"
    ${DX_UNASSOC} "wux"   "Disc Image"
    ${DX_UNASSOC} "wud"   "Disc Image"
    ${DX_UNASSOC} "fatx"  "Disc Image"
    ${DX_UNASSOC} "scram" "Disc Image"
    ${DX_UNASSOC} "sdram" "Disc Image"
    ${DX_UNASSOC} "sbram" "Disc Image"
    !undef DX_UNASSOC

    ; Without this Explorer keeps drawing the old icons until it restarts, which
    ; looks like the choice was ignored.
    !insertmacro UPDATEFILEASSOC

  DiscXplorer_KeepAssoc:
!macroend
