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

FIRST VIEWPORT: Flyout ~380px wide above the taskbar. Top band: pictogram square left, one-line instruction, time left monumental (~40px). Below, one row per device: pictogram, name + via/state, percent right-aligned with time left under it. Footer: alert threshold, History, Settings. Taskbar: yellow tag only for the device needing action, others plain white text.

FORM: Challenger wayfinding-cartography-signage-terminal-yellow-wayfinding (chosen over the assigned Studio Light); seed:30bde16ef1905471. Signature move: split-flap band update when urgency changes; instant swap under reduced motion.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Reference

Approved sketch: .impeccable/mocks/decision/challenger-airport.html (synthetic data).

## Open decisions

Exact critical-state treatment; light-theme rendering of the band; app name.
