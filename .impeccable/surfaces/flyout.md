---
version: 1
slug: "flyout"
primary_target: "flyout"
related_targets: []
---

## Scope

Taskbar presence + flyout panel of BatteryLife (Windows 11). Mode: Operate. Build path: code-led.

## Audience and job

PC gamer between matches: one glance tells which device to charge and how much time is left. Read-only device data; no device control.

## States

All good (no action), action needed (at or below alert threshold), critical (5% or less), charging, asleep (last value dimmed), disconnected, no devices found, unknown level.

## Direction contract

THESIS: The hub is an airport sign, not a dashboard: it states the single next decision ("Charge the headset · 2 h 10 min"). It refuses the category default of a grid of equal battery bars.

OWN-WORLD: Signal yellow #ffd200 panel with black type, used only when an action exists; black #111 ground, white type, #2a2a2a 1px separators; white pictograms on black squares, one stroke weight. Segoe UI Variable (humanist, Frutiger lineage), tabular figures, sentence case.

STORY: The user sees yellow only when something needs charging, reads the device and time left in under a second, and trusts the calm black state otherwise.

FIRST VIEWPORT: Flyout ~380px wide above the taskbar. Top band: pictogram square left, one-line instruction, time left monumental (~40px). Below, one row per device: pictogram, name + via/state, percent right-aligned with time left under it; a device that needs charging carries its level as a yellow tag. Footer: alert threshold, Refresh, Settings. Taskbar: the tray icon shows the level of the device that runs out first, on yellow only when it needs charging.

FORM: Challenger wayfinding-cartography-signage-terminal-yellow-wayfinding (chosen over the assigned Studio Light); seed:30bde16ef1905471. Signature move: split-flap update of the whole band plate when urgency changes; instant swap under reduced motion.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Platform adaptations (v1)

- Taskbar tag became a tray icon with the number drawn in it: Windows offers no supported API for text on the taskbar (BatteryHub's text strip is a child window injected into Explorer). New tray icons start in the ^ overflow and no app may pin itself, so the first run shows a notification with a "Pin to taskbar" button that opens ms-settings:taskbar.
- Critical (5% or less) keeps the yellow band; the headline says "now" ("Carregue o headset agora"). No extra color: yellow stays the only signal color.
- Contrast themes map the sign onto the user's system colors (ThemeDictionaries HighContrast).

## Reference

Approved sketch: .impeccable/mocks/decision/challenger-airport.html (synthetic data).

## Deferred (not in v1, not promised by this surface)

Battery history view, battery wear/health display, a configurable alert threshold (fixed at 15%), the age of an asleep reading ("2 h ago": the core does not report last-seen yet).

## Open decisions

Light-theme rendering of the band; app name.
