; "DELETE THE APPLICATION DATA" MUST DELETE ANTILOKI'S DATA (2026-10-11, the operator on a Windows PC that had antiloki before: he ticked the box and the old
; workspaces were all still there, so the onboarding could not be tested). Tauri's own uninstall step removes only $APPDATA\dev.antiloki.desktop and
; $LOCALAPPDATA\dev.antiloki.desktop (the web view's storage); antiloki keeps its data in three other places:
;   $LOCALAPPDATA\antiloki      the workspace registry and every database (and the trial's shadow)
;   $APPDATA\antiloki           the licence record
;   $PROFILE\.antiloki          settings, the benchmark record, forge tokens, agent boxes
; An update (UpdateMode = 1) never reaches this: it keeps everything. Project folders are never touched (their own .antiloki folders stay in the repo).
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    RmDir /r "$LOCALAPPDATA\antiloki"
    RmDir /r "$APPDATA\antiloki"
    RmDir /r "$PROFILE\.antiloki"
  ${EndIf}
!macroend
