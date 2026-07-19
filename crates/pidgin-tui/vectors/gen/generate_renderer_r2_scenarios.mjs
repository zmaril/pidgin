// straitjacket-allow-file:duplication — extracted scenario data for the PR-R2
// overlay/focus vector generator; the per-scenario objects are uniform data
// mirroring pi's overlay tests, not shared logic.
//
// Scenario data for generate_renderer_r2.mjs. Components are addressed by index.
// Overlay handles referenced by ops/reactions use the 0-based order of
// showOverlay calls in the scenario (handles[0] is the first overlay shown).

// Base content helpers rendered at the scenario's fixed terminal width.
const styledLine80 = `\x1b[1m\x1b[38;2;255;0;0m${"X".repeat(80)}\x1b[0m`;
const hyperlink = "\x1b]8;;file:///path/to/file.ts\x07file.ts\x1b]8;;\x07";
const hyperlinkLine80 = `See ${hyperlink} for details ${"X".repeat(80 - 30)}`;
const complexLine =
	"\x1b[48;2;40;50;40m \x1b[38;2;128;128;128mSome styled content\x1b[39m\x1b[49m" +
	"\x1b]8;;http://example.com\x07link\x1b]8;;\x07" +
	" more content ".repeat(10);

// An overlay-compositing scenario: one base component + one or more overlay
// components, shown, then a forced full render captured byte-exact.
function overlayCase(name, { columns = 80, rows = 24, base, overlays }) {
	const components = base.concat(overlays.map((o) => ({ lines: o.lines })));
	const steps = [{ op: "start" }];
	overlays.forEach((o, i) => {
		steps.push({ op: "showOverlay", component: base.length + i, options: o.options });
	});
	steps.push({ op: "render", force: true });
	steps.push({ op: "stop" });
	return { name, columns, rows, components, base: base.map((_, i) => i), steps };
}

export const compositingScenarios = [
	overlayCase("overlay_top_left", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["TOP-LEFT"], options: { anchor: "top-left", width: 10 } }],
	}),
	overlayCase("overlay_bottom_right", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["BTM-RIGHT"], options: { anchor: "bottom-right", width: 10 } }],
	}),
	overlayCase("overlay_top_center", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["CENTERED"], options: { anchor: "top-center", width: 10 } }],
	}),
	overlayCase("overlay_margin_number", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["MARGIN"], options: { anchor: "top-left", width: 10, margin: 5 } }],
	}),
	overlayCase("overlay_margin_object", {
		base: [{ lines: [] }],
		overlays: [
			{ lines: ["MARGIN"], options: { anchor: "top-left", width: 10, margin: { top: 2, left: 3, right: 0, bottom: 0 } } },
		],
	}),
	overlayCase("overlay_offset", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["OFFSET"], options: { anchor: "top-left", width: 10, offsetX: 10, offsetY: 5 } }],
	}),
	overlayCase("overlay_pct_center", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["PCT"], options: { width: 10, row: "50%", col: "50%" } }],
	}),
	overlayCase("overlay_row_pct_0", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["TOP"], options: { width: 10, row: "0%" } }],
	}),
	overlayCase("overlay_row_pct_100", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["BOTTOM"], options: { width: 10, row: "100%" } }],
	}),
	overlayCase("overlay_max_height", {
		base: [{ lines: [] }],
		overlays: [
			{ lines: ["Line 1", "Line 2", "Line 3", "Line 4", "Line 5"], options: { maxHeight: 3 } },
		],
	}),
	overlayCase("overlay_max_height_pct", {
		rows: 10,
		base: [{ lines: [] }],
		overlays: [
			{ lines: ["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10"], options: { maxHeight: "50%" } },
		],
	}),
	overlayCase("overlay_absolute", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["ABSOLUTE"], options: { anchor: "bottom-right", row: 3, col: 5, width: 10 } }],
	}),
	overlayCase("overlay_width_pct", {
		columns: 100,
		base: [{ lines: [] }],
		overlays: [{ lines: ["test"], options: { width: "50%" } }],
	}),
	overlayCase("overlay_min_width", {
		columns: 100,
		base: [{ lines: [] }],
		overlays: [{ lines: ["test"], options: { width: "10%", minWidth: 30 } }],
	}),
	overlayCase("overlay_width_overflow_truncate", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["X".repeat(100)], options: { width: 20 } }],
	}),
	overlayCase("overlay_edge_col60", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["X".repeat(50)], options: { col: 60, width: 20 } }],
	}),
	overlayCase("overlay_complex_ansi", {
		base: [{ lines: [] }],
		overlays: [{ lines: [complexLine, complexLine, complexLine], options: { width: 60 } }],
	}),
	overlayCase("overlay_styled_base", {
		base: [{ lines: [styledLine80, styledLine80, styledLine80] }],
		overlays: [{ lines: ["OVERLAY"], options: { width: 20, anchor: "center" } }],
	}),
	overlayCase("overlay_wide_chars", {
		base: [{ lines: [] }],
		overlays: [{ lines: ["中文日本語한글テスト漢字"], options: { width: 15 } }],
	}),
	overlayCase("overlay_osc_base", {
		base: [{ lines: [hyperlinkLine80, hyperlinkLine80, hyperlinkLine80] }],
		overlays: [{ lines: ["OVERLAY-TEXT"], options: { anchor: "center", width: 20 } }],
	}),
	overlayCase("overlay_short_content", {
		base: [{ lines: ["Line 1", "Line 2", "Line 3"] }],
		overlays: [{ lines: ["OVERLAY_TOP", "OVERLAY_MID", "OVERLAY_BOT"], options: undefined }],
	}),
	overlayCase("overlay_style_leak", {
		columns: 20,
		rows: 6,
		base: [{ lines: [`\x1b[3m${"X".repeat(20)}\x1b[23m`, "INPUT"] }],
		overlays: [{ lines: ["OVR"], options: { row: 0, col: 5, width: 3 } }],
	}),
	overlayCase("overlay_stacked_top", {
		base: [{ lines: [] }],
		overlays: [
			{ lines: ["FIRST-OVERLAY"], options: { anchor: "top-left", width: 20 } },
			{ lines: ["SECOND"], options: { anchor: "top-left", width: 10 } },
		],
	}),
	overlayCase("overlay_different_positions", {
		base: [{ lines: [] }],
		overlays: [
			{ lines: ["TOP-LEFT"], options: { anchor: "top-left", width: 15 } },
			{ lines: ["BTM-RIGHT"], options: { anchor: "bottom-right", width: 15 } },
		],
	}),
];

// Stacked-hide: show two overlays then hideOverlay, re-render — verifies z-order
// on removal (the "should properly hide overlays in stack order" test).
compositingScenarios.push({
	name: "overlay_hide_stack_order",
	columns: 80,
	rows: 24,
	components: [{ lines: [] }, { lines: ["FIRST"] }, { lines: ["SECOND"] }],
	base: [0],
	steps: [
		{ op: "start" },
		{ op: "showOverlay", component: 1, options: { anchor: "top-left", width: 10 } },
		{ op: "showOverlay", component: 2, options: { anchor: "top-left", width: 10 } },
		{ op: "render", force: true },
		{ op: "hideOverlay" },
		{ op: "render", force: true },
		{ op: "stop" },
	],
});

// --- Focus-restore scenarios ---
// Components: 0 = empty base; the rest are focusable editors/overlays. Editors
// are standalone focus targets (not base children), so their lines never reach
// the write stream; only overlays composite. Handles referenced by showOverlay
// order (0-based).

export const focusScenarios = [
	{
		name: "focus_non_capturing_preserves",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["OVERLAY"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_transfer_and_unfocus",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["OVERLAY"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlayFocus", handle: 0 },
			{ op: "render", force: true },
			{ op: "overlayUnfocus", handle: 0, hasOptions: false },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_set_hidden_no_autofocus",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["OVERLAY"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlaySetHidden", handle: 0, hidden: true },
			{ op: "overlaySetHidden", handle: 0, hidden: false },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_hide_when_focused_restores",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["OVERLAY"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlayFocus", handle: 0 },
			{ op: "overlayHide", handle: 0 },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_capturing_removed_nc_below_restores_editor",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["NC"] }, { lines: ["CAP"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "showOverlay", component: 3, options: undefined },
			{ op: "overlayHide", handle: 1 },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_sub_overlay_cleanup_then_hideoverlay",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["TIMER"] }, { lines: ["CTRL"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "showOverlay", component: 3, options: undefined },
			{ op: "overlayHide", handle: 0 },
			{ op: "hideOverlay" },
			{ op: "render", force: true },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_removed_child_not_parent_fallback",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["CHILD"] }, { lines: ["PARENT"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlayFocus", handle: 0 },
			{ op: "showOverlay", component: 3, options: undefined },
			{ op: "overlayHide", handle: 0 },
			{ op: "overlayHide", handle: 1 },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_redirect_skips_nc_when_invisible",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["FALLBACK"] }, { lines: ["NC"] }, { lines: ["PRIMARY"] }],
		base: [0],
		flags: [true],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: undefined },
			{ op: "showOverlay", component: 3, options: { nonCapturing: true } },
			{ op: "showOverlay", component: 4, options: { visibleFlag: 0 } },
			{ op: "setFlag", flag: 0, value: false },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_active_base_replacement_close_before_restore",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["REPLACEMENT"] }, { lines: ["OVERLAY"] }],
		base: [0],
		reactions: [
			{ component: 3, data: "b", actions: [{ op: "setFocus", target: 2 }] },
			{ component: 2, data: "\r", actions: [{ op: "setFocus", target: 1 }] },
		],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 3, options: undefined },
			{ op: "sendInput", data: "b" },
			{ op: "render", force: true },
			{ op: "sendInput", data: "\r" },
			{ op: "render", force: true },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_active_replacement_other_overlay_prefocus",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["REPLACEMENT"] }, { lines: ["PASSIVE"] }, { lines: ["OVERLAY"] }],
		base: [0],
		reactions: [
			{ component: 4, data: "b", actions: [{ op: "setFocus", target: 2 }] },
			{ component: 2, data: "\r", actions: [{ op: "setFocus", target: 1 }] },
		],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "setFocus", target: 2 },
			{ op: "showOverlay", component: 3, options: { nonCapturing: true } },
			{ op: "setFocus", target: 1 },
			{ op: "showOverlay", component: 4, options: undefined },
			{ op: "sendInput", data: "b" },
			{ op: "render", force: true },
			{ op: "sendInput", data: "1" },
			{ op: "sendInput", data: "\r" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_unfocus_target_releases_blocked",
		columns: 80,
		rows: 24,
		components: [
			{ lines: [] },
			{ lines: ["FALLBACK"] },
			{ lines: ["TARGET"] },
			{ lines: ["REPLACEMENT"] },
			{ lines: ["OVERLAY"] },
		],
		base: [0],
		reactions: [
			{
				component: 4,
				data: "b",
				actions: [
					{ op: "setFocus", target: 3 },
					{ op: "unfocusTarget", handle: 0, target: 2 },
				],
			},
			{ component: 3, data: "\r", actions: [{ op: "setFocus", target: 1 }] },
		],
		steps: [
			{ op: "start" },
			{ op: "showOverlay", component: 4, options: undefined },
			{ op: "sendInput", data: "b" },
			{ op: "render", force: true },
			{ op: "sendInput", data: "\r" },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_restore_visible_overlay_after_base_steal",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["REPLACEMENT"] }, { lines: ["OVERLAY"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 3, options: undefined },
			{ op: "setFocus", target: 2 },
			{ op: "setFocus", target: 1 },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_passive_nc_no_regain_input",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["NC"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_explicit_nc_regains_input",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["NC"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlayFocus", handle: 0 },
			{ op: "setFocus", target: 1 },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_unfocus_prevents_regain",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["NC"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlayFocus", handle: 0 },
			{ op: "overlayUnfocus", handle: 0, hasOptions: false },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_set_null_clears_visible_restore",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["NC"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlayFocus", handle: 0 },
			{ op: "setFocus", target: null },
			{ op: "sendInput", data: "x" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_hideoverlay_topmost_nc_no_reassign",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["CAP"] }, { lines: ["NC"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: undefined },
			{ op: "showOverlay", component: 3, options: { nonCapturing: true } },
			{ op: "hideOverlay" },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_capturing_unfocus_falls_back_prefocus",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["CAP"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: undefined },
			{ op: "overlayUnfocus", handle: 0, hasOptions: false },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
	{
		name: "focus_noop_guards",
		columns: 80,
		rows: 24,
		components: [{ lines: [] }, { lines: ["EDITOR"] }, { lines: ["OVERLAY"] }],
		base: [0],
		steps: [
			{ op: "setFocus", target: 1 },
			{ op: "start" },
			{ op: "showOverlay", component: 2, options: { nonCapturing: true } },
			{ op: "overlaySetHidden", handle: 0, hidden: true },
			{ op: "overlayFocus", handle: 0 },
			{ op: "overlaySetHidden", handle: 0, hidden: false },
			{ op: "overlayHide", handle: 0 },
			{ op: "overlayFocus", handle: 0 },
			{ op: "overlayUnfocus", handle: 0, hasOptions: false },
			{ op: "render", force: true },
			{ op: "stop" },
		],
	},
];
