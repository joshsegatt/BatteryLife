# Product

<!-- impeccable:product-schema 1 -->

## Platform

windows

Native Windows 11 desktop app (taskbar presence + flyout panel). Not web, not mobile.

## Stack

Rust core (device engine: HID, Bluetooth, polling) + WinUI 3 shell for the visible surfaces. Chosen by the user from a recommendation. Note: WinUI 3 has no first-class Rust projection, so the shell is expected to be C# or C++/WinRT talking to the Rust core; exact bridge undecided.

## Users

PC gamers on Windows 11 with several wireless peripherals (mouse, headset, keyboard, controller, earbuds, phone). They glance at battery levels between matches or before a session, and want to know before a device dies mid-game. First user is the author (Josh), a dev and gamer.

## Product Purpose

A modern battery and device-health hub: shows the level of every wireless device at a glance, charging state, estimated time left, history, battery wear, and alerts. Success: the user never gets surprised by a dead device, and checking it takes one glance at the taskbar or one click.

## Positioning

The reference app (BatteryHub, GPL-3) proves the device coverage but shows only plain text "icon 93%" on the taskbar through an undocumented hack. This product aims to be the polished, modern hub: same read-only safety, richer health insight (time left, wear, history), and a first-class Windows 11 visual experience. Open source under GPL-3; driver protocol logic may be ported from BatteryHub with credit.

## Operating Context

- Lives in the taskbar all day; opened with one click into a flyout panel (like Windows 11 Quick Settings).
- Used in dark rooms, gaming setups, often with the dark Windows theme, sometimes with transparent/custom taskbars.
- Multi-monitor setups and fullscreen games are common.

## Capabilities and Constraints

- Read-only access to devices: never writes settings (no DPI/RGB control). Confirmed scope: battery + health.
- Surfaces confirmed: taskbar presence + flyout panel. Game Bar overlay, full window and Windows Widgets are not in the first scope.
- Must respect light/dark theme, DPI scaling, multi-monitor, and accessibility (contrast, keyboard, screen reader).
- Undecided: app name/brand (working name "BatteryLife"), monetization (Store paid build is possible under GPL), which devices ship first (depends on the author's own hardware).

## Brand Commitments

- The user asked for a more "cinematic" and modern look than BatteryHub, explicitly not copying its visuals.
- Avoid generic AI looks (purple-cyan gradients, neon glass, generic 3-card grids, bouncing).

## Evidence on Hand

- Reference repo studied (BatteryHub, ~100 vendor drivers, GPL-3).
- Screenshot of BatteryHub's taskbar strip ("mouse 93%  phone 84%") as the anti-reference.
- No real users, metrics or testimonials yet. Do not invent any.

## Product Principles

1. One glance is enough: the most urgent device is always the most visible.
2. Never touch the device: read-only, always safe.
3. Truthful data: show "unknown" or "asleep" honestly; never fake precision.
4. Native first: feel like part of Windows 11, not a skin on top of it.
5. Quiet until it matters: calm at rest, loud only when a battery is about to die.

## Accessibility & Inclusion

Battery state must never rely on color alone (low battery uses icon + text too). Respect Windows contrast themes, text scaling and reduced motion.
