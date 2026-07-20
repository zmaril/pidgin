# coding-agent STARTED frontier — missing symbols (wave lane)

**Provenance:** pidgin `7756e93`, pi `3da591ab`, hinzu port-diff run (`scratchpad/hinzu-run/portdiff-coding-agent.json`). Working-tree HEAD observed at generation time: pidgin `677288c`, pi submodule `3da591a`.

**Method for "missing":** The port-diff per-file records carry only a `tier_breakdown` count (`exact_module`/`subtree`/`global_name`/`unmatched`) — there is **no per-symbol tier array**. So "missing" is derived by the **grep fallback**: pi symbols for each file are enumerated from `src-coding-agent.graph.json` (`symbols[].file == path`, excluding `<module>` and anonymous `(callback)` entries), and each is grepped (`grep -rwl --include=*.rs`) across `crates/pidgin-coding/src` and `crates/pidgin-cli/src` in its snake_case / original / SCREAMING_SNAKE form (fn/const) or PascalCase form (type). Zero hits ⇒ **MISSING**.

> **Caveat:** grep-based "missing" is best-effort. A symbol that was **renamed** during the port reads as missing (false positive), and a symbol whose name **collides** with an unrelated Rust identifier reads as present (false negative). Counts here therefore differ from the port-diff `matched_symbols` / `tier_breakdown.unmatched`, which are graph-alignment based. `graph_syms` below counts named pi symbols only and will differ from the port-diff `total_symbols` (which includes anonymous callbacks).

## STARTED frontier (ordered by fan_in)

| src | fan_in | coverage | matched/total | grep_missing/graph_syms | mapped_target |
|---|---:|---:|---:|---:|---|
| packages/coding-agent/src/core/extensions/types.ts | 118 | 0.46 | 64/139 | 65/139 | core/extensions/types |
| packages/coding-agent/src/modes/interactive/theme/theme.ts | 104 | 0.455 | 61/134 | 26/121 | modes/interactive/theme |
| packages/coding-agent/src/core/agent-session.ts | 71 | 0.513 | 78/152 | 7/13 | core/agent_session |
| packages/coding-agent/src/config.ts | 38 | 0.093 | 4/43 | 35/43 | core/auth/auth_guidance |
| packages/coding-agent/src/core/sdk.ts | 23 | 0.4 | 4/10 | 2/10 | core/sdk |
| packages/coding-agent/src/core/extensions/runner.ts | 13 | 0.378 | 45/119 | 29/45 | core/extensions/runner |
| packages/coding-agent/src/core/tools/bash.ts | 12 | 0.452 | 14/31 | 10/31 | core/tools/bash |
| packages/coding-agent/src/core/extensions/loader.ts | 11 | 0.596 | 31/52 | 21/52 | core/extensions/loader |
| packages/coding-agent/src/core/tools/edit.ts | 8 | 0.276 | 8/29 | 16/29 | core/tools/edit |
| packages/coding-agent/src/core/tools/read.ts | 8 | 0.174 | 4/23 | 14/23 | core/tools/read |
| packages/coding-agent/src/utils/child-process.ts | 7 | 0.462 | 6/13 | 5/13 | utils/child_process |
| packages/coding-agent/src/core/tools/file-mutation-queue.ts | 6 | 0.333 | 1/3 | 1/3 | core/tools/file_mutation_queue |
| packages/coding-agent/src/cli/startup-ui.ts | 5 | 0.059 | 1/17 | 13/17 | — |
| packages/coding-agent/src/core/footer-data-provider.ts | 5 | 0.321 | 9/28 | 5/9 | core/footer_data_provider |
| packages/coding-agent/src/core/tools/find.ts | 5 | 0.313 | 5/16 | 7/16 | core/tools/find |
| packages/coding-agent/src/core/event-bus.ts | 4 | 0.571 | 4/7 | 4/7 | core/event_bus |
| packages/coding-agent/src/core/output-guard.ts | 4 | 0.111 | 1/9 | 2/9 | core/output_guard |
| packages/coding-agent/src/core/timings.ts | 4 | 0.167 | 1/6 | 1/6 | core/timings |
| packages/coding-agent/src/modes/interactive/components/custom-editor.ts | 4 | 0.25 | 1/4 | 1/1 | — |
| packages/coding-agent/src/modes/interactive/components/session-selector.ts | 4 | 0.18 | 11/61 | 13/17 | core |
| packages/coding-agent/src/utils/clipboard-image.ts | 4 | 0.071 | 1/14 | 13/14 | harness/env/nodejs |
| packages/coding-agent/src/utils/pi-user-agent.ts | 4 | 0.0 | 0/1 | 1/1 | utils/pi_user_agent |
| packages/coding-agent/src/core/tools/grep.ts | 3 | 0.3 | 6/20 | 11/20 | core/tools/grep |
| packages/coding-agent/src/main.ts | 3 | 0.08 | 2/25 | 9/25 | core |
| packages/coding-agent/src/modes/interactive/components/bordered-loader.ts | 3 | 0.429 | 3/7 | 1/1 | core/agent_session |
| packages/coding-agent/src/modes/interactive/components/countdown-timer.ts | 3 | 0.333 | 1/3 | 0/1 | core/agent_session |
| packages/coding-agent/src/modes/interactive/components/tool-execution.ts | 3 | 0.542 | 13/24 | 0/2 | modes/interactive/components/tool_execution |
| packages/coding-agent/src/modes/interactive/components/user-message.ts | 3 | 0.5 | 3/6 | 0/1 | modes/interactive/components/user_message |
| packages/coding-agent/src/modes/rpc/jsonl.ts | 3 | 0.2 | 1/5 | 2/5 | modes/rpc/jsonl |
| packages/coding-agent/src/modes/rpc/rpc-mode.ts | 3 | 0.382 | 21/55 | 28/55 | core |
| packages/coding-agent/src/utils/image-resize.ts | 3 | 0.111 | 1/9 | 7/9 | auth/oauth/xai |
| packages/coding-agent/src/utils/tools-manager.ts | 3 | 0.2 | 3/15 | 4/15 | utils/tools_manager |
| packages/coding-agent/src/core/export-html/tool-renderer.ts | 2 | 0.3 | 3/10 | 2/10 | core/export_html/tool_renderer |
| packages/coding-agent/src/extensions/llama/index.ts | 2 | 0.067 | 1/15 | 12/15 | modes/interactive/app |
| packages/coding-agent/src/modes/interactive/components/bash-execution.ts | 2 | 0.455 | 5/11 | 1/1 | modes/interactive/components/tool_execution |
| packages/coding-agent/src/modes/interactive/components/config-selector.ts | 2 | 0.111 | 7/63 | 14/16 | core |
| packages/coding-agent/src/modes/interactive/components/extension-input.ts | 2 | 0.333 | 2/6 | 2/2 | core/agent_session |
| packages/coding-agent/src/modes/interactive/components/extension-selector.ts | 2 | 0.333 | 2/6 | 2/2 | core/agent_session |
| packages/coding-agent/src/modes/interactive/components/login-dialog.ts | 2 | 0.133 | 2/15 | 1/1 | auth/types |
| packages/coding-agent/src/modes/interactive/components/model-selector.ts | 2 | 0.222 | 4/18 | 4/4 | providers/registry |
| packages/coding-agent/src/modes/interactive/components/oauth-selector.ts | 2 | 0.111 | 1/9 | 3/3 | — |
| packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts | 2 | 0.15 | 3/20 | 6/11 | core/timings |
| packages/coding-agent/src/modes/interactive/components/session-selector-search.ts | 2 | 0.083 | 1/12 | 11/12 | — |
| packages/coding-agent/src/modes/interactive/components/status-indicator.ts | 2 | 0.444 | 8/18 | 1/8 | modes/interactive/components/status_indicator |
| packages/coding-agent/src/modes/interactive/components/tree-selector.ts | 2 | 0.259 | 15/58 | 13/13 | components/settings_list |
| packages/coding-agent/src/modes/interactive/components/trust-selector.ts | 2 | 0.125 | 1/8 | 4/4 | — |
| packages/coding-agent/src/modes/interactive/interactive-mode.ts | 2 | 0.138 | 41/298 | 18/19 | core |
| packages/coding-agent/src/modes/rpc/rpc-types.ts | 2 | 0.286 | 2/7 | 5/7 | modes/rpc/types |
| packages/coding-agent/src/package-manager-cli.ts | 2 | 0.167 | 7/42 | 28/42 | modes/interactive/theme/runtime |
| packages/coding-agent/src/utils/exif-orientation.ts | 2 | 0.455 | 5/11 | 1/11 | utils/exif |
| packages/coding-agent/src/utils/windows-self-update.ts | 2 | 0.2 | 1/5 | 4/5 | utils |
| packages/coding-agent/src/core/extensions/wrapper.ts | 1 | 0.333 | 1/3 | 1/3 | core/tools/bash |
| packages/coding-agent/src/core/provider-attribution.ts | 1 | 0.571 | 4/7 | 3/7 | core/provider_attribution |
| packages/coding-agent/src/modes/interactive/components/armin.ts | 1 | 0.182 | 4/22 | 5/5 | widgets/loader |
| packages/coding-agent/src/modes/interactive/components/branch-summary-message.ts | 1 | 0.5 | 3/6 | 1/1 | modes/interactive/components/tool_execution |
| packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts | 1 | 0.5 | 3/6 | 1/1 | modes/interactive/components/tool_execution |
| packages/coding-agent/src/modes/interactive/components/custom-entry.ts | 1 | 0.5 | 3/6 | 1/1 | modes/interactive/components/tool_execution |
| packages/coding-agent/src/modes/interactive/components/custom-message.ts | 1 | 0.5 | 3/6 | 1/1 | modes/interactive/components/tool_execution |
| packages/coding-agent/src/modes/interactive/components/daxnuts.ts | 1 | 0.273 | 3/11 | 3/4 | core/agent_session |
| packages/coding-agent/src/modes/interactive/components/extension-editor.ts | 1 | 0.333 | 2/6 | 1/1 | core/settings_manager/manager |
| packages/coding-agent/src/modes/interactive/components/first-time-setup.ts | 1 | 0.125 | 1/8 | 3/3 | — |
| packages/coding-agent/src/modes/interactive/components/settings-selector.ts | 1 | 0.207 | 6/29 | 10/10 | core |
| packages/coding-agent/src/modes/interactive/components/skill-invocation-message.ts | 1 | 0.5 | 3/6 | 1/1 | modes/interactive/components/tool_execution |
| packages/coding-agent/src/modes/interactive/components/user-message-selector.ts | 1 | 0.333 | 3/9 | 3/3 | — |

## Priority files (full missing-symbol detail)

### src/config.ts — PRIORITY
- band **STARTED**, fan_in **38**, port-diff matched **4/43**, mapped_target **core/auth/auth_guidance**, tier_breakdown {"exact_module": 0, "subtree": 2, "global_name": 2, "unmatched": 39}
- named pi symbols: **43**, grep-present: **8**, grep-missing: **35**
- MISSING (grep) symbols:
  - `getSelfUpdateCommand` — fn/const (pi fan_in 14)
  - `detectInstallMethod` — fn/const (pi fan_in 13)
  - `SelfUpdateCommand` — type (pi fan_in 5)
  - `SelfUpdatePackageTarget` — type (pi fan_in 5)
  - `InstallMethod` — type (pi fan_in 4)
  - `getCustomThemesDir` — fn/const (pi fan_in 3)
  - `getSelfUpdateCommandForMethod` — fn/const (pi fan_in 3)
  - `getSelfUpdateUnavailableInstruction` — fn/const (pi fan_in 3)
  - `getUpdateInstruction` — fn/const (pi fan_in 3)
  - `SelfUpdateCommandStep` — type (pi fan_in 2)
  - `expandTildePath` — fn/const (pi fan_in 2)
  - `getChangelogPath` — fn/const (pi fan_in 2)
  - `getInferredNpmInstall` — fn/const (pi fan_in 2)
  - `getPathComparisonCandidates` — fn/const (pi fan_in 2)
  - `getThemesDir` — fn/const (pi fan_in 2)
  - `isManagedByGlobalPackageManager` — fn/const (pi fan_in 2)
  - `isSelfUpdatePathWritable` — fn/const (pi fan_in 2)
  - `normalizeSelfUpdatePackageTarget` — fn/const (pi fan_in 2)
  - `readCommandOutput` — fn/const (pi fan_in 2)
  - `getBundledInteractiveAssetPath` — fn/const (pi fan_in 1)
  - `getDebugLogPath` — fn/const (pi fan_in 1)
  - `getEntrypointPackageDir` — fn/const (pi fan_in 1)
  - `getExportTemplateDir` — fn/const (pi fan_in 1)
  - `getGlobalPackageRoots` — fn/const (pi fan_in 1)
  - `getInteractiveAssetsDir` — fn/const (pi fan_in 1)
  - `getPackageJsonPath` — fn/const (pi fan_in 1)
  - `getSettingsPath` — fn/const (pi fan_in 1)
  - `getShareViewerUrl` — fn/const (pi fan_in 1)
  - `makeSelfUpdateCommand` — fn/const (pi fan_in 1)
  - `makeSelfUpdateCommandStep` — fn/const (pi fan_in 1)
  - `normalizeExistingPathForComparison` — fn/const (pi fan_in 1)
  - `PackageJson` — type (pi fan_in 0)
  - `getModelsPath` — fn/const (pi fan_in 0)
  - `getPromptsDir` — fn/const (pi fan_in 0)
  - `getToolsDir` — fn/const (pi fan_in 0)

### src/utils/paths.ts — PRIORITY (note: port-diff band is PORTED, 7/8, 1 unmatched; grep matched all 8 by name)
- band **PORTED**, fan_in **25**, port-diff matched **7/8**, mapped_target **utils/paths**, tier_breakdown {"exact_module": 7, "subtree": 0, "global_name": 0, "unmatched": 1}
- named pi symbols: **8**, grep-present: **8**, grep-missing: **0**
- MISSING (grep): none — all named pi symbols matched by name in the pidgin tree.

## Top-15 STARTED files by fan_in (full missing-symbol detail)

### src/core/extensions/types.ts 
- band **STARTED**, fan_in **118**, port-diff matched **64/139**, mapped_target **core/extensions/types**, tier_breakdown {"exact_module": 3, "subtree": 0, "global_name": 61, "unmatched": 75}
- named pi symbols: **139**, grep-present: **74**, grep-missing: **65**
- MISSING (grep) symbols:
  - `ExtensionFactory` — type (pi fan_in 6)
  - `WorkingIndicatorOptions` — type (pi fan_in 5)
  - `ExtensionUIDialogOptions` — type (pi fan_in 4)
  - `EntryRenderer` — type (pi fan_in 3)
  - `MessageRenderer` — type (pi fan_in 3)
  - `ExtensionFlag` — type (pi fan_in 2)
  - `ExtensionWidgetOptions` — type (pi fan_in 2)
  - `ProviderConfig` — type (pi fan_in 2)
  - `defineTool` — fn/const (pi fan_in 2)
  - `AnyToolDefinition` — type (pi fan_in 1)
  - `AutocompleteProviderFactory` — type (pi fan_in 1)
  - `BashToolResultEvent` — type (pi fan_in 1)
  - `EditToolCallEvent` — type (pi fan_in 1)
  - `EditToolResultEvent` — type (pi fan_in 1)
  - `EditorFactory` — type (pi fan_in 1)
  - `ExtensionActions` — type (pi fan_in 1)
  - `ExtensionContextActions` — type (pi fan_in 1)
  - `ExtensionShortcut` — type (pi fan_in 1)
  - `FindToolCallEvent` — type (pi fan_in 1)
  - `FindToolResultEvent` — type (pi fan_in 1)
  - `GrepToolCallEvent` — type (pi fan_in 1)
  - `GrepToolResultEvent` — type (pi fan_in 1)
  - `InlineExtension` — type (pi fan_in 1)
  - `LsToolCallEvent` — type (pi fan_in 1)
  - `LsToolResultEvent` — type (pi fan_in 1)
  - `ReadToolResultEvent` — type (pi fan_in 1)
  - `WriteToolCallEvent` — type (pi fan_in 1)
  - `WriteToolResultEvent` — type (pi fan_in 1)
  - `AppendEntryHandler` — type (pi fan_in 0)
  - `CompactOptions` — type (pi fan_in 0)
  - `CustomToolCallEvent` — type (pi fan_in 0)
  - `CustomToolResultEvent` — type (pi fan_in 0)
  - `EntryRenderOptions` — type (pi fan_in 0)
  - `ExtensionHandler` — type (pi fan_in 0)
  - `ExtensionRuntimeState` — type (pi fan_in 0)
  - `GetActiveToolsHandler` — type (pi fan_in 0)
  - `GetAllToolsHandler` — type (pi fan_in 0)
  - `GetCommandsHandler` — type (pi fan_in 0)
  - `GetSessionNameHandler` — type (pi fan_in 0)
  - `GetThinkingLevelHandler` — type (pi fan_in 0)
  - `HandlerFn` — type (pi fan_in 0)
  - `MessageRenderOptions` — type (pi fan_in 0)
  - `ProjectTrustHandler` — type (pi fan_in 0)
  - `ProviderModelConfig` — type (pi fan_in 0)
  - `RefreshToolsHandler` — type (pi fan_in 0)
  - `SendMessageHandler` — type (pi fan_in 0)
  - `SendUserMessageHandler` — type (pi fan_in 0)
  - `SessionEvent` — type (pi fan_in 0)
  - `SetActiveToolsHandler` — type (pi fan_in 0)
  - `SetLabelHandler` — type (pi fan_in 0)
  - `SetModelHandler` — type (pi fan_in 0)
  - `SetSessionNameHandler` — type (pi fan_in 0)
  - `SetThinkingLevelHandler` — type (pi fan_in 0)
  - `TerminalInputHandler` — type (pi fan_in 0)
  - `ToolCallEventBase` — type (pi fan_in 0)
  - `ToolResultEventBase` — type (pi fan_in 0)
  - `WidgetPlacement` — type (pi fan_in 0)
  - `isBashToolResult` — fn/const (pi fan_in 0)
  - `isEditToolResult` — fn/const (pi fan_in 0)
  - `isFindToolResult` — fn/const (pi fan_in 0)
  - `isGrepToolResult` — fn/const (pi fan_in 0)
  - `isLsToolResult` — fn/const (pi fan_in 0)
  - `isReadToolResult` — fn/const (pi fan_in 0)
  - `isToolCallEventType` — fn/const (pi fan_in 0)
  - `isWriteToolResult` — fn/const (pi fan_in 0)

### src/modes/interactive/theme/theme.ts 
- band **STARTED**, fan_in **104**, port-diff matched **61/134**, mapped_target **modes/interactive/theme**, tier_breakdown {"exact_module": 0, "subtree": 49, "global_name": 12, "unmatched": 73}
- named pi symbols: **121**, grep-present: **95**, grep-missing: **26**
- MISSING (grep) symbols:
  - `setRegisteredThemes` — fn/const (pi fan_in 6)
  - `getLanguageFromPath` — fn/const (pi fan_in 4)
  - `getSettingsListTheme` — fn/const (pi fan_in 4)
  - `setGlobalTheme` — fn/const (pi fan_in 4)
  - `ThemeInfo` — type (pi fan_in 3)
  - `getAvailableThemes` — fn/const (pi fan_in 3)
  - `getAvailableThemesWithPaths` — fn/const (pi fan_in 3)
  - `getEditorTheme` — fn/const (pi fan_in 3)
  - `getThemeByName` — fn/const (pi fan_in 3)
  - `CliHighlightTheme` — type (pi fan_in 2)
  - `getBuiltinThemes` — fn/const (pi fan_in 2)
  - `getCliHighlightTheme` — fn/const (pi fan_in 2)
  - `resolveThemeColors` — fn/const (pi fan_in 2)
  - `TerminalAutoThemeDetectionOptions` — type (pi fan_in 1)
  - `TerminalBackgroundThemeDetectionOptions` — type (pi fan_in 1)
  - `TerminalThemeDetectionOptions` — type (pi fan_in 1)
  - `addTheme` — fn/const (pi fan_in 1)
  - `buildCliHighlightTheme` — fn/const (pi fan_in 1)
  - `getAnsiColorLuminance` — fn/const (pi fan_in 1)
  - `getCustomThemeInfos` — fn/const (pi fan_in 1)
  - `deletion` — fn/const (pi fan_in 0)
  - `doctag` — fn/const (pi fan_in 0)
  - `emphasis` — fn/const (pi fan_in 0)
  - `hr` — fn/const (pi fan_in 0)
  - `operator` — fn/const (pi fan_in 0)
  - `regexp` — fn/const (pi fan_in 0)

### src/core/agent-session.ts 
- band **STARTED**, fan_in **71**, port-diff matched **78/152**, mapped_target **core/agent_session**, tier_breakdown {"exact_module": 0, "subtree": 68, "global_name": 10, "unmatched": 74}
- named pi symbols: **13**, grep-present: **6**, grep-missing: **7**
- MISSING (grep) symbols:
  - `ModelCycleResult` — type (pi fan_in 3)
  - `withoutDeletedHeaders` — fn/const (pi fan_in 3)
  - `ParsedSkillBlock` — type (pi fan_in 2)
  - `SessionStats` — type (pi fan_in 2)
  - `estimateMessagesTokens` — fn/const (pi fan_in 2)
  - `ExtensionBindings` — type (pi fan_in 1)
  - `ToolDefinitionEntry` — type (pi fan_in 0)

### src/config.ts — see Priority section above

### src/core/sdk.ts 
- band **STARTED**, fan_in **23**, port-diff matched **4/10**, mapped_target **core/sdk**, tier_breakdown {"exact_module": 4, "subtree": 0, "global_name": 0, "unmatched": 6}
- named pi symbols: **10**, grep-present: **8**, grep-missing: **2**
- MISSING (grep) symbols:
  - `convertToLlmWithBlockImages` — fn/const (pi fan_in 0)
  - `transformHeaders` — fn/const (pi fan_in 0)

### src/core/extensions/runner.ts 
- band **STARTED**, fan_in **13**, port-diff matched **45/119**, mapped_target **core/extensions/runner**, tier_breakdown {"exact_module": 19, "subtree": 0, "global_name": 26, "unmatched": 74}
- named pi symbols: **45**, grep-present: **16**, grep-missing: **29**
- MISSING (grep) symbols:
  - `BuiltInKeyBindings` — type (pi fan_in 1)
  - `RunnerEmitEvent` — type (pi fan_in 1)
  - `SessionBeforeEvent` — type (pi fan_in 1)
  - `buildBuiltinKeybindings` — fn/const (pi fan_in 1)
  - `ForkHandler` — type (pi fan_in 0)
  - `NavigateTreeHandler` — type (pi fan_in 0)
  - `NewSessionHandler` — type (pi fan_in 0)
  - `ReloadHandler` — type (pi fan_in 0)
  - `SessionBeforeEventResult` — type (pi fan_in 0)
  - `ShutdownHandler` — type (pi fan_in 0)
  - `SwitchSessionHandler` — type (pi fan_in 0)
  - `addAutocompleteProvider` — fn/const (pi fan_in 0)
  - `getAllThemes` — fn/const (pi fan_in 0)
  - `getEditorComponent` — fn/const (pi fan_in 0)
  - `getEditorText` — fn/const (pi fan_in 0)
  - `getToolsExpanded` — fn/const (pi fan_in 0)
  - `onTerminalInput` — fn/const (pi fan_in 0)
  - `pasteToEditor` — fn/const (pi fan_in 0)
  - `setEditorComponent` — fn/const (pi fan_in 0)
  - `setEditorText` — fn/const (pi fan_in 0)
  - `setFooter` — fn/const (pi fan_in 0)
  - `setHeader` — fn/const (pi fan_in 0)
  - `setStatus` — fn/const (pi fan_in 0)
  - `setTitle` — fn/const (pi fan_in 0)
  - `setToolsExpanded` — fn/const (pi fan_in 0)
  - `setWidget` — fn/const (pi fan_in 0)
  - `setWorkingIndicator` — fn/const (pi fan_in 0)
  - `setWorkingMessage` — fn/const (pi fan_in 0)
  - `setWorkingVisible` — fn/const (pi fan_in 0)

### src/core/tools/bash.ts 
- band **STARTED**, fan_in **12**, port-diff matched **14/31**, mapped_target **core/tools/bash**, tier_breakdown {"exact_module": 11, "subtree": 0, "global_name": 3, "unmatched": 17}
- named pi symbols: **31**, grep-present: **21**, grep-missing: **10**
- MISSING (grep) symbols:
  - `clearUpdateTimer` — fn/const (pi fan_in 3)
  - `BashResultRenderComponent` — type (pi fan_in 2)
  - `BashRenderState` — type (pi fan_in 1)
  - `formatBashCall` — fn/const (pi fan_in 1)
  - `formatDuration` — fn/const (pi fan_in 1)
  - `onAbort` — fn/const (pi fan_in 1)
  - `rebuildBashResultRenderComponent` — fn/const (pi fan_in 1)
  - `scheduleOutputUpdate` — fn/const (pi fan_in 1)
  - `BashResultRenderState` — type (pi fan_in 0)
  - `handleData` — fn/const (pi fan_in 0)

### src/core/extensions/loader.ts 
- band **STARTED**, fan_in **11**, port-diff matched **31/52**, mapped_target **core/extensions/loader**, tier_breakdown {"exact_module": 2, "subtree": 0, "global_name": 29, "unmatched": 21}
- named pi symbols: **52**, grep-present: **31**, grep-missing: **21**
- MISSING (grep) symbols:
  - `ExtensionCacheToken` — type (pi fan_in 4)
  - `clearExtensionCache` — fn/const (pi fan_in 4)
  - `loadExtensionFromFactory` — fn/const (pi fan_in 3)
  - `createExtension` — fn/const (pi fan_in 2)
  - `createExtensionAPI` — fn/const (pi fan_in 2)
  - `loadExtensionsInternal` — fn/const (pi fan_in 2)
  - `HandlerFn` — type (pi fan_in 1)
  - `getAliases` — fn/const (pi fan_in 1)
  - `isCurrentCacheToken` — fn/const (pi fan_in 1)
  - `loadExtension` — fn/const (pi fan_in 1)
  - `loadExtensionModule` — fn/const (pi fan_in 1)
  - `resolveWorkspaceOrImport` — fn/const (pi fan_in 1)
  - `useExtensionCacheCwd` — fn/const (pi fan_in 1)
  - `assertActive` — fn/const (pi fan_in 0)
  - `getFlag` — fn/const (pi fan_in 0)
  - `notInitialized` — fn/const (pi fan_in 0)
  - `on` — fn/const (pi fan_in 0)
  - `registerEntryRenderer` — fn/const (pi fan_in 0)
  - `registerFlag` — fn/const (pi fan_in 0)
  - `registerMessageRenderer` — fn/const (pi fan_in 0)
  - `registerShortcut` — fn/const (pi fan_in 0)

### src/core/tools/edit.ts 
- band **STARTED**, fan_in **8**, port-diff matched **8/29**, mapped_target **core/tools/edit**, tier_breakdown {"exact_module": 4, "subtree": 0, "global_name": 4, "unmatched": 21}
- named pi symbols: **29**, grep-present: **13**, grep-missing: **16**
- MISSING (grep) symbols:
  - `createEditTool` — fn/const (pi fan_in 12)
  - `RenderableEditArgs` — type (pi fan_in 4)
  - `EditPreview` — type (pi fan_in 3)
  - `EditToolInput` — type (pi fan_in 3)
  - `EditRenderState` — type (pi fan_in 2)
  - `buildEditCallComponent` — fn/const (pi fan_in 2)
  - `getRenderablePreviewInput` — fn/const (pi fan_in 2)
  - `setEditPreview` — fn/const (pi fan_in 2)
  - `EditOperations` — type (pi fan_in 1)
  - `EditToolDetails` — type (pi fan_in 1)
  - `EditToolResultLike` — type (pi fan_in 1)
  - `createEditCallRenderComponent` — fn/const (pi fan_in 1)
  - `getEditCallRenderComponent` — fn/const (pi fan_in 1)
  - `getEditHeaderBg` — fn/const (pi fan_in 1)
  - `LegacyEditToolInput` — type (pi fan_in 0)
  - `readFile` — fn/const (pi fan_in 0)

### src/core/tools/read.ts 
- band **STARTED**, fan_in **8**, port-diff matched **4/23**, mapped_target **core/tools/read**, tier_breakdown {"exact_module": 0, "subtree": 0, "global_name": 4, "unmatched": 19}
- named pi symbols: **23**, grep-present: **9**, grep-missing: **14**
- MISSING (grep) symbols:
  - `ReadRenderArgs` — type (pi fan_in 5)
  - `CompactReadClassification` — type (pi fan_in 3)
  - `formatReadLineRange` — fn/const (pi fan_in 2)
  - `ReadOperations` — type (pi fan_in 1)
  - `formatCompactReadCall` — fn/const (pi fan_in 1)
  - `formatReadCall` — fn/const (pi fan_in 1)
  - `formatReadResult` — fn/const (pi fan_in 1)
  - `getCompactReadClassification` — fn/const (pi fan_in 1)
  - `getNonVisionImageNote` — fn/const (pi fan_in 1)
  - `getPiDocsClassification` — fn/const (pi fan_in 1)
  - `trimTrailingEmptyLines` — fn/const (pi fan_in 1)
  - `ReadToolInput` — type (pi fan_in 0)
  - `onAbort` — fn/const (pi fan_in 0)
  - `readFile` — fn/const (pi fan_in 0)

### src/utils/child-process.ts 
- band **STARTED**, fan_in **7**, port-diff matched **6/13**, mapped_target **utils/child_process**, tier_breakdown {"exact_module": 3, "subtree": 0, "global_name": 3, "unmatched": 7}
- named pi symbols: **13**, grep-present: **8**, grep-missing: **5**
- MISSING (grep) symbols:
  - `armIdleTimer` — fn/const (pi fan_in 2)
  - `cleanup` — fn/const (pi fan_in 2)
  - `onExit` — fn/const (pi fan_in 0)
  - `onStderrEnd` — fn/const (pi fan_in 0)
  - `onStdoutEnd` — fn/const (pi fan_in 0)

### src/core/tools/file-mutation-queue.ts 
- band **STARTED**, fan_in **6**, port-diff matched **1/3**, mapped_target **core/tools/file_mutation_queue**, tier_breakdown {"exact_module": 1, "subtree": 0, "global_name": 0, "unmatched": 2}
- named pi symbols: **3**, grep-present: **2**, grep-missing: **1**
- MISSING (grep) symbols:
  - `isMissingPathError` — fn/const (pi fan_in 1)

### src/cli/startup-ui.ts 
- band **STARTED**, fan_in **5**, port-diff matched **1/17**, mapped_target **—**, tier_breakdown {"exact_module": 0, "subtree": 0, "global_name": 1, "unmatched": 16}
- named pi symbols: **17**, grep-present: **4**, grep-missing: **13**
- MISSING (grep) symbols:
  - `shouldRunFirstTimeSetup` — fn/const (pi fan_in 6)
  - `createStartupTui` — fn/const (pi fan_in 4)
  - `clearStartupTui` — fn/const (pi fan_in 3)
  - `showStartupSelector` — fn/const (pi fan_in 3)
  - `startStartupTui` — fn/const (pi fan_in 3)
  - `DistributionMetadata` — type (pi fan_in 1)
  - `applyDetectedStartupTheme` — fn/const (pi fan_in 1)
  - `isOfficialDistribution` — fn/const (pi fan_in 1)
  - `loadStartupThemes` — fn/const (pi fan_in 1)
  - `showFirstTimeSetup` — fn/const (pi fan_in 1)
  - `showSetup` — fn/const (pi fan_in 1)
  - `showStartupInput` — fn/const (pi fan_in 1)
  - `onThemePreview` — fn/const (pi fan_in 0)

### src/core/footer-data-provider.ts 
- band **STARTED**, fan_in **5**, port-diff matched **9/28**, mapped_target **core/footer_data_provider**, tier_breakdown {"exact_module": 8, "subtree": 0, "global_name": 1, "unmatched": 19}
- named pi symbols: **9**, grep-present: **4**, grep-missing: **5**
- MISSING (grep) symbols:
  - `ReadonlyFooterDataProvider` — type (pi fan_in 3)
  - `isWindowsMountedRepoPath` — fn/const (pi fan_in 1)
  - `isWslEnvironment` — fn/const (pi fan_in 1)
  - `resolveBranchWithGitAsync` — fn/const (pi fan_in 1)
  - `shouldPollGitHead` — fn/const (pi fan_in 1)

### src/core/tools/find.ts 
- band **STARTED**, fan_in **5**, port-diff matched **5/16**, mapped_target **core/tools/find**, tier_breakdown {"exact_module": 0, "subtree": 0, "global_name": 5, "unmatched": 11}
- named pi symbols: **16**, grep-present: **9**, grep-missing: **7**
- MISSING (grep) symbols:
  - `createFindTool` — fn/const (pi fan_in 5)
  - `FindToolDetails` — type (pi fan_in 2)
  - `cleanup` — fn/const (pi fan_in 2)
  - `formatFindCall` — fn/const (pi fan_in 1)
  - `formatFindResult` — fn/const (pi fan_in 1)
  - `FindToolInput` — type (pi fan_in 0)
  - `onAbort` — fn/const (pi fan_in 0)

## Remaining STARTED files 16–64 (missing counts only)

Full per-symbol detail computed for all 64 files in the JSON; listed here as counts only.

| src | fan_in | grep_missing/graph_syms |
|---|---:|---:|
| packages/coding-agent/src/core/event-bus.ts | 4 | 4/7 |
| packages/coding-agent/src/core/output-guard.ts | 4 | 2/9 |
| packages/coding-agent/src/core/timings.ts | 4 | 1/6 |
| packages/coding-agent/src/modes/interactive/components/custom-editor.ts | 4 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/session-selector.ts | 4 | 13/17 |
| packages/coding-agent/src/utils/clipboard-image.ts | 4 | 13/14 |
| packages/coding-agent/src/utils/pi-user-agent.ts | 4 | 1/1 |
| packages/coding-agent/src/core/tools/grep.ts | 3 | 11/20 |
| packages/coding-agent/src/main.ts | 3 | 9/25 |
| packages/coding-agent/src/modes/interactive/components/bordered-loader.ts | 3 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/countdown-timer.ts | 3 | 0/1 |
| packages/coding-agent/src/modes/interactive/components/tool-execution.ts | 3 | 0/2 |
| packages/coding-agent/src/modes/interactive/components/user-message.ts | 3 | 0/1 |
| packages/coding-agent/src/modes/rpc/jsonl.ts | 3 | 2/5 |
| packages/coding-agent/src/modes/rpc/rpc-mode.ts | 3 | 28/55 |
| packages/coding-agent/src/utils/image-resize.ts | 3 | 7/9 |
| packages/coding-agent/src/utils/tools-manager.ts | 3 | 4/15 |
| packages/coding-agent/src/core/export-html/tool-renderer.ts | 2 | 2/10 |
| packages/coding-agent/src/extensions/llama/index.ts | 2 | 12/15 |
| packages/coding-agent/src/modes/interactive/components/bash-execution.ts | 2 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/config-selector.ts | 2 | 14/16 |
| packages/coding-agent/src/modes/interactive/components/extension-input.ts | 2 | 2/2 |
| packages/coding-agent/src/modes/interactive/components/extension-selector.ts | 2 | 2/2 |
| packages/coding-agent/src/modes/interactive/components/login-dialog.ts | 2 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/model-selector.ts | 2 | 4/4 |
| packages/coding-agent/src/modes/interactive/components/oauth-selector.ts | 2 | 3/3 |
| packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts | 2 | 6/11 |
| packages/coding-agent/src/modes/interactive/components/session-selector-search.ts | 2 | 11/12 |
| packages/coding-agent/src/modes/interactive/components/status-indicator.ts | 2 | 1/8 |
| packages/coding-agent/src/modes/interactive/components/tree-selector.ts | 2 | 13/13 |
| packages/coding-agent/src/modes/interactive/components/trust-selector.ts | 2 | 4/4 |
| packages/coding-agent/src/modes/interactive/interactive-mode.ts | 2 | 18/19 |
| packages/coding-agent/src/modes/rpc/rpc-types.ts | 2 | 5/7 |
| packages/coding-agent/src/package-manager-cli.ts | 2 | 28/42 |
| packages/coding-agent/src/utils/exif-orientation.ts | 2 | 1/11 |
| packages/coding-agent/src/utils/windows-self-update.ts | 2 | 4/5 |
| packages/coding-agent/src/core/extensions/wrapper.ts | 1 | 1/3 |
| packages/coding-agent/src/core/provider-attribution.ts | 1 | 3/7 |
| packages/coding-agent/src/modes/interactive/components/armin.ts | 1 | 5/5 |
| packages/coding-agent/src/modes/interactive/components/branch-summary-message.ts | 1 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts | 1 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/custom-entry.ts | 1 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/custom-message.ts | 1 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/daxnuts.ts | 1 | 3/4 |
| packages/coding-agent/src/modes/interactive/components/extension-editor.ts | 1 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/first-time-setup.ts | 1 | 3/3 |
| packages/coding-agent/src/modes/interactive/components/settings-selector.ts | 1 | 10/10 |
| packages/coding-agent/src/modes/interactive/components/skill-invocation-message.ts | 1 | 1/1 |
| packages/coding-agent/src/modes/interactive/components/user-message-selector.ts | 1 | 3/3 |
