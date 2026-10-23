---
type: Guide
title: "18-LAB design system for tool UIs"
description: "Layout, copy and component rules shared by the vosu dashboard and every 18-LAB tool, with the class reference for /tools/_host/v1/theme.css."
---

# 18-LAB design system for tool UIs

The vosu dashboard and every first-party tool use one calm design system: the 18-LAB palette (WORKSTATION graphite and amber, DRAFTING paper and blueprint), Barlow for the interface, Barlow Condensed only for the brand and large numeric readouts, and IBM Plex Mono for data, indices and code. It is served by vosu, so tools always match the running dashboard:

```html
<link rel="stylesheet" href="/tools/_host/v1/theme.css">
```

`theme.css` is the dashboard's own `tokens.css` followed by the component classes in `system.css`; there is no second copy to drift. The bridge sets `data-theme` on `<html>` from the dashboard's mode (`vosu.colorScheme`), so a tool needs no theme code. With Vite, import it with `/* @vite-ignore */` or link it from `index.html` so the build does not try to bundle it. Do not fork or override the tokens. The live reference with every component in both modes is served at [`/tools/_host/v1/design`](http://127.0.0.1:24050/tools/_host/v1/design).

## Rules

**Copy.** Labels only. A tool's purpose is on its dashboard card, so the tool page has no title block, tagline, explanatory paragraph, caption, footnote, provenance or licence blurb, or "how to" list. Descriptions belong in `settings.json` (shown in the settings dialog). Add a help hover (`.v-help`) only when a label is genuinely ambiguous; if it is self-explanatory, leave it out. Empty states are one short line. Sentence case everywhere; mono caps only for small section labels and data headings.

**An app, not a page.** `<body class="v-app">` holds a 40 px `.v-bar` and a `.v-main`. The page itself never scrolls at 1920×1080, 2560×1440, 1280×720 or half-width 960×1080: lay panels out to fill the viewport and give only unbounded lists a `.v-scroll` region inside their panel. No hero banners or big headers; the bar shows the tool name, its live status, and its controls. At narrow widths (down to 390 px where the tool's nature allows) panels stack, and the main area may scroll.

**Density and type.** Controls are 32 px (`--control-h`), small ones 26 px. Body text is 14 px Barlow; weights stay at 400–600. No condensed bold uppercase headings.

**Chrome is not content.** Bars, buttons, labels, tabs and table headers are not selectable; values, tables and logs are. Every interactive element shows the shared focus ring.

**Native controls.** Use `.v-select` (its popup follows the mode through `color-scheme`), and never a raw `<input type="file">`: wrap it in a `.v-file` button label. Checkboxes, radios and ranges take the accent colour.

**Audio.** Previews that play sound start muted and keep a per-tool mute toggle (`.v-toggle` or an icon button with `aria-pressed`), remembered between visits.

**Live data.** Follow the game with `vosu.subscribe()` and show its state with a `.v-dot` in the bar; read options with `vosu.settings()`.

## Class reference

All classes are prefixed `v-`. Modifiers use `--`; parts use `__`.

### App shell

| Class | Use |
| --- | --- |
| `.v-app` | On `<body>`: viewport-high grid of bar and main; the page does not scroll. |
| `.v-bar`, `.v-bar__title`, `.v-bar__spacer` | Fixed 40 px top bar with a 2 px accent rule: name, status, flexible space, controls. |
| `.v-main` | Work area under the bar; padded grid with an 8 px gap. |
| `.v-split` | Side panel (15–20rem) plus work area; stacks below 48rem. |
| `.v-grid` | Responsive tiles; set `--v-min` to the smallest tile width. |
| `.v-row`, `.v-stack` | Wrapping horizontal row; vertical stack. |
| `.v-scroll` | Internal scroll region (lists, tables, logs). |

```html
<body class="v-app">
  <header class="v-bar">
    <span class="v-bar__title">Mania ratings</span>
    <span class="v-dot" data-state="ok"></span><span class="v-muted">Live</span>
    <span class="v-bar__spacer"></span>
    <button class="v-btn v-btn--ghost v-btn--icon" aria-label="Settings">⚙</button>
  </header>
  <main class="v-main v-split">
    <section class="v-panel">…</section>
    <section class="v-panel">…</section>
  </main>
</body>
```

### Panels and text

| Class | Use |
| --- | --- |
| `.v-panel`, `.v-panel__head`, `.v-panel__body` | Raised region with a 32 px head (label left, actions right) and a padded body; add `.v-scroll` to the body for long content. |
| `.v-label` | Mono caps section label; wrap an order index in `<b>`: `<span class="v-label"><b>01</b> / Library</span>`. |
| `.v-title` | Sentence-case heading inside a panel, used sparingly. |
| `.v-toolbar`, `.v-toolbar__sep` | Row of controls with a thin separator. |
| `.v-muted`, `.v-mono`, `.v-num` | Secondary text; tabular mono; right-aligned tabular numbers. |

### Buttons

| Class | Use |
| --- | --- |
| `.v-btn` | Secondary action. |
| `.v-btn--primary` | The one main action of a region (filled accent). |
| `.v-btn--ghost` | Low-emphasis action, e.g. in the bar. |
| `.v-btn--danger` | Destructive action. |
| `.v-btn--icon` | Square icon button; always give it `aria-label`. |
| `.v-btn--sm` | 26 px variant. |
| `[aria-pressed="true"]` | Toggle state on any `.v-btn`. |

```html
<button class="v-btn v-btn--primary">Generate map</button>
<button class="v-btn">Preview</button>
<button class="v-btn v-btn--icon" aria-label="Mute" aria-pressed="true">🔇</button>
```

### Fields and choices

| Class | Use |
| --- | --- |
| `.v-field`, `.v-field--inline` | Label above (or beside) a control: `<label class="v-field"><span>Rate</span><input class="v-input" type="number"></label>`. |
| `.v-input`, `.v-input--mono` | Text, search and number inputs (numbers are tabular mono). |
| `.v-select` | Native select with a themed popup. |
| `.v-textarea` | Multi-line input. |
| `.v-file` | File picker: `<label class="v-btn v-file"><input type="file" accept=".osr">Choose replay</label>`. |
| `.v-check` | Checkbox or radio with its label. |
| `.v-toggle` | Switch: `<input type="checkbox" role="switch" class="v-toggle">`. |
| `.v-range` | Range slider. |
| `.v-seg` | Segmented control: buttons with `aria-pressed`, one pressed. |
| `.v-tabs` | `role="tablist"` of `role="tab"` buttons with `aria-selected`. |

Invalid inputs take `aria-invalid="true"`.

### Data and status

| Class | Use |
| --- | --- |
| `.v-table` | Compact table; `th`/`td` with `.v-num` for numbers; `tr[aria-selected="true"]` for the selected row; sticky header inside `.v-scroll`. |
| `.v-stat`, `.v-stat__label`, `.v-stat__value`, `.v-stat__unit`, `.v-stat--sm` | Numeric readout. |
| `.v-meter` | Bar: `<div class="v-meter" style="--v-value: 0.62"></div>`; `--v-color` overrides the fill. |
| `.v-dot` | Status dot: `data-state` `ok`, `busy`, `warn`, `error`, or none for idle; pair it with a text label. |
| `.v-badge`, `--accent`, `--ok`, `--warn`, `--error` | Small mono chip for versions, counts and states. |
| `.v-facts` | Key/value `<dl>`, e.g. in a drawer. |

### Help, empty state, toast, drawer

| Class | Use |
| --- | --- |
| `.v-help` | Help hover for an ambiguous label: `<span class="v-help" tabindex="0" role="img" aria-label="Seconds between window starts." data-tip="Seconds between window starts."></span>`; `data-tip-pos="left"` near the right edge. |
| `.v-empty` | One short centred line: `<p class="v-empty">No replays yet.</p>`. |
| `.v-toast` | `role="status"` message at the bottom right; `data-tone="ok"` or `"error"`. |
| `.v-drawer`, `.v-drawer__head`, `.v-drawer__body` | Side panel `<dialog>` opened with `showModal()`, for details and secondary settings. |

### Tokens

Use the custom properties from `theme.css` instead of literal colours: `--bg`, `--bg-raised`, `--bg-sunken`, `--rule`, `--rule-strong`, `--fg`, `--fg-dim`, `--muted`, `--accent`, `--on-accent`, `--signal`, `--note`, `--caution`, `--danger`, `--ok`, `--hover`, `--selected`, `--scrim`, `--shadow`, `--grid-minor`, `--grid-major`; type `--font-body`, `--font-display`, `--font-mono`, `--step--2` … `--step-3`; spacing `--space-1` … `--space-8`; sizes `--control-h`, `--control-h-sm`, `--bar-h`, `--radius`. Canvas and chart code can read them with `getComputedStyle(document.documentElement)`. Each mode sets `color-scheme`, so scrollbars and native popups follow it.
