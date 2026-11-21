---
name: BatteryLife
description: An airport sign for your wireless devices, living in the Windows 11 taskbar.
colors:
  sign-yellow: "#FFD200"
  sign-ink: "#111111"
  sign-ground: "#111111"
  sign-raised: "#1C1C1C"
  sign-rule: "#2A2A2A"
  sign-text: "#FFFFFF"
  sign-muted: "#A8A8A8"
typography:
  figure:
    fontFamily: "Segoe UI Variable Display"
    fontSize: "40px"
    fontWeight: 700
    lineHeight: "44px"
    letterSpacing: "-0.02em"
  figure-unit:
    fontFamily: "Segoe UI Variable Display"
    fontSize: "24px"
    fontWeight: 600
  level:
    fontFamily: "Segoe UI Variable Display"
    fontSize: "22px"
    fontWeight: 600
  level-tag:
    fontFamily: "Segoe UI Variable Display"
    fontSize: "20px"
    fontWeight: 700
  headline:
    fontFamily: "Segoe UI Variable Text"
    fontSize: "15px"
    fontWeight: 600
  title:
    fontFamily: "Segoe UI Variable Text"
    fontSize: "14px"
    fontWeight: 600
  body:
    fontFamily: "Segoe UI Variable Text"
    fontSize: "12px"
    fontWeight: 400
  caption:
    fontFamily: "Segoe UI Variable Text"
    fontSize: "11.5px"
    fontWeight: 400
rounded:
  plate: "0px"
  tray-icon: "6px"
  window: "8px"
spacing:
  xxs: "2px"
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "14px"
  gutter: "18px"
components:
  band-action:
    backgroundColor: "{colors.sign-yellow}"
    textColor: "{colors.sign-ink}"
    rounded: "{rounded.plate}"
    padding: "18px 18px 20px 18px"
  band-calm:
    backgroundColor: "{colors.sign-raised}"
    textColor: "{colors.sign-text}"
    rounded: "{rounded.plate}"
    padding: "18px 18px 20px 18px"
  band-pictogram-action:
    backgroundColor: "{colors.sign-ink}"
    textColor: "{colors.sign-text}"
    rounded: "{rounded.plate}"
    size: "56px"
  band-pictogram-calm:
    backgroundColor: "{colors.sign-rule}"
    textColor: "{colors.sign-text}"
    rounded: "{rounded.plate}"
    size: "56px"
  device-row:
    backgroundColor: "{colors.sign-ground}"
    textColor: "{colors.sign-text}"
    rounded: "{rounded.plate}"
    padding: "12px 18px"
  row-pictogram:
    backgroundColor: "{colors.sign-raised}"
    textColor: "{colors.sign-text}"
    rounded: "{rounded.plate}"
    size: "36px"
  level-tag:
    backgroundColor: "{colors.sign-yellow}"
    textColor: "{colors.sign-ink}"
    typography: "{typography.level-tag}"
    rounded: "{rounded.plate}"
    padding: "1px 7px 2px 7px"
  footer:
    backgroundColor: "{colors.sign-ground}"
    textColor: "{colors.sign-muted}"
    typography: "{typography.body}"
    padding: "6px 10px 8px 18px"
  tray-icon-action:
    backgroundColor: "{colors.sign-yellow}"
    textColor: "{colors.sign-ink}"
    rounded: "{rounded.tray-icon}"
    size: "64px"
  tray-icon-calm:
    backgroundColor: "{colors.sign-raised}"
    textColor: "{colors.sign-text}"
    rounded: "{rounded.tray-icon}"
    size: "64px"
---

# Design System: BatteryLife

## Overview

**Creative North Star: "The Airport Sign"**

BatteryLife is wayfinding signage, not a dashboard. The flyout states one next decision ("Carregue o headset antes de jogar · 2 h 10 min") on a single plate at the top, and everything below it is a quiet departure board of devices. The world is built from three materials: a black ground, square plates, and one signal yellow that appears only when something has to be charged. When nothing needs charging the whole surface is black and white, and that calm is itself information.

Density is that of a Windows 11 flyout (Quick Settings): a fixed 380px column, sized to its content, rising from the taskbar. Hierarchy comes from scale and weight rather than color: the time left is set monumental (40px bold) with its units smaller and lighter, so the number reads first, as on a departure board. Pictograms are white on dark squares, one icon family, never colored.

The surface refuses the category default of a grid of equal battery bars, and refuses the purple-cyan gradient, neon glass and bouncing motion the product brief rules out. Motion is mechanical: the band turns over like a split-flap plate when the decision changes, and the panel rises a short distance like Quick Settings.

**Key Characteristics:**
- One signal color (yellow), present only when an action exists.
- Square plates inside a native rounded Windows frame.
- The figure is the hero: number at full weight, units lighter.
- White pictograms on dark squares, one icon family.
- Sentence-case instructions in pt-BR or English, middle dots (·) as separators.
- Mechanical motion (split-flap), instant under reduced motion.

## Colors

A black-and-white sign system with one signal yellow held in reserve.

### Primary
- **Signal Yellow** (sign-yellow): the "act now" plate. Fills the top band when a device is at or below the alert threshold (15%) or critical (5% or less), the level tag of the row that needs charging, and the tray icon background in the same condition. Also the app icon ground. Never decorative, never a hover, never a brand accent in a calm state.

### Neutral
- **Sign Ink** (sign-ink): type and the pictogram square on a yellow plate. Same value as the ground, kept as a separate role because the contrast theme maps it differently (highlight text).
- **Night Ground** (sign-ground): the panel background behind rows and footer.
- **Raised Plate** (sign-raised): the calm band, row pictogram squares, and the calm tray icon. One step above the ground, so the band still reads as a plate without color.
- **Hairline Rule** (sign-rule): 1px separators between rows and above the footer, the Windows frame hairline, and the band pictogram square in the calm state.
- **Sign White** (sign-text): primary text, figures, and every pictogram, always.
- **Quiet Grey** (sign-muted): secondary lines (device kind · connection or state), the row's time left, the footer.

### Contrast theme mapping
Under a Windows contrast theme the sign maps onto the user's system colors (ThemeDictionaries `HighContrast`): yellow becomes SystemColorHighlight, ink becomes SystemColorHighlightText, ground and raised become SystemColorWindow, and rule, text and muted all become SystemColorWindowText. The "act now" plate therefore stays the one highlighted thing on screen. Code-built rows read the same brushes (through `Palette.Brush`) and repaint when the theme changes.

### Named Rules
**The Act-Now Rule.** In the panel and tray, yellow appears only when a device needs charging, and only in three places: the band, that device's level tag, and the tray icon. If nothing needs charging, no yellow is on screen.

**The One Signal Rule.** Critical does not get a second color. It keeps the yellow band and says "agora" / "now" in the headline. Urgency escalates in words, not in red.

**The Never Color Alone Rule.** Every yellow state also carries words ("carregar agora", "Carregue o headset") and a level figure, so the state survives color blindness and contrast themes.

## Typography

**Display Font:** Segoe UI Variable Display (figures and levels)
**Body Font:** Segoe UI Variable Text (instructions, names, secondary lines)
**Pictogram Font:** Segoe Fluent Icons (the Windows icon set)

**Character:** The Frutiger-lineage humanist sans that Windows 11 itself speaks, used at signage contrast: big bold numbers, small calm words.

### Hierarchy
- **Figure** (700, 40px, 44px line, -0.02em): the band's time left or level. Units inside it ("h", "min", "dias") drop to the **Figure unit** role (600, 24px).
- **Headline** (600, 15px, up to 2 lines): the band's one-line instruction, sentence case.
- **Level** (600, 22px): a row's percent, right-aligned. When the row needs charging it becomes the **Level tag** (700, 20px) on yellow.
- **Title** (600, 14px): device name in a row.
- **Body** (400, 12px): band detail line, row subtitle, footer.
- **Caption** (400, 11.5px): the row's time left under its level.

### Named Rules
**The Number First Rule.** In any figure, digits are set larger and heavier than their units, so the eye lands on the quantity.

**The Sentence Case Rule.** All copy is sentence case, no all-caps labels, parts joined with a middle dot ("Mouse · Logitech receiver"). Instructions are imperative and name the device ("Carregue o headset agora").

## Layout

A single fixed column 380px wide, height sized to content, anchored bottom-right of the work area of the monitor under the cursor with a 12px margin, like Quick Settings. Three stacked regions: the band (one plate), one row per device in first-seen order, and a footer.

Spacing runs on an 18px outer gutter. The band pads 18px on three sides and 20px at the bottom, with a 56px pictogram column and a 14px gap; its text stack uses 4px gaps. Rows pad 12px vertically and 18px horizontally with a 36px pictogram column, a flexible name column and an auto-width level column, separated by 12px. Inside a row, stacks use 2px (name over subtitle) and 4px (charging glyph beside level). The footer pads 6px top, 8px bottom, 18px left and 10px right, with two 8px-padded icon buttons.

## Elevation & Depth

Flat. There are no shadows inside the panel; depth is tonal: ground (sign-ground) below, raised plates (sign-raised) one step up, and 1px hairlines (sign-rule) between rows. The only lift is the Windows frame itself (system flyout shadow from the context-menu presenter), always on top like Quick Settings.

### Named Rules
**The Tonal Plate Rule.** Separation comes from a one-step tone change or a 1px hairline, never from a drop shadow inside the panel.

## Shapes

Plates are square. The band, rows, pictogram squares and the level tag have no corner radius, like enamel signage. Rounding belongs only to Windows-owned silhouettes: the window frame uses the system round corner (8px, DWMWCP_ROUND) with a hairline border in the rule color; the tray icon is a 64px tile with a 6px radius; the app icon is a yellow tile with corners at 18% of its size carrying a black battery outline.

## Components

### Sign band (signature component)
The single next decision. A 56px pictogram square on the left, then headline, figure and detail stacked.
- **Action / critical:** yellow plate, ink type, ink pictogram square, white pictogram.
- **Calm:** raised plate, white type, rule-grey pictogram square, white pictogram. Figure reads "21 h" or "Tudo certo".
- **No devices / reader down:** calm plate, no figure, the detail says what to do.
- **Split-flap motion:** when the headline changes and the panel is open, the plate folds to -90 degrees (110ms, exponential ease-in), every value swaps at the fold, then it returns from 90 degrees to flat (220ms, exponential ease-out). When Windows animation effects are off, the swap is instant.
- **Accessibility:** a polite live region whose name is headline, figure and detail together.

### Device row
- **Structure:** 36px raised pictogram square, name (Title) over kind · state (Body, muted), level right-aligned with time left beneath (Caption, muted).
- **Separator:** 1px rule at the top of each row.
- **Needs charging:** the level becomes a yellow tag (square, 1/7/2/7px padding, ink Level tag type) and the subtitle reads "carregar agora".
- **Charging:** a white charging glyph (14px) precedes the level; time left is hidden.
- **Asleep:** pictogram and level drop to 55% opacity; name and subtitle keep full contrast, subtitle reads "dormindo · último valor".

### Footer
- **Style:** 1px rule on top, alert threshold in muted Body on the left ("Alerta em 15%"), refresh and settings as transparent, borderless 14px pictogram buttons on the right with native WinUI hover and focus.
- **Settings:** a native MenuFlyout (start with Windows, exit), not a custom menu.

### Tray icon
- **Style:** a 64px generated tile carrying the level of the device that runs out first in Segoe UI Variable Display Bold (40px for one character, 34px for two, 26px for three); "!" when the reader is down, "–" when unknown.
- **Action:** yellow tile, ink number. **Calm:** raised tile, white number.

### Taskbar strip
The main devices inside the taskbar itself, just left of the tray icons, always visible: each device's pictogram and level, like a departures line.
- **Order:** fixed by kind (mouse, headset, keyboard, controller, phone, other), then name, never by level, so each device keeps its place. Up to 4.
- **Style:** Segoe Fluent pictogram (16px at 100% scale), 4px gap, level in Segoe UI Variable Text Bold 14px; 6px padding per device, 2px between devices. No background: the taskbar shows through, and the ink follows the taskbar theme (white on a dark taskbar, #111111 on a light one). Hover draws a faint rounded highlight like the tray's own buttons.
- **Action:** a square sign-yellow plate (28px high) behind that one device, ink pictogram and level: the same rule as the band and the row tag. **Charging:** a bolt glyph (11px) before the level. **Asleep:** pictogram and level at 55% opacity, like the row.
- **Room:** it never covers an app button. When the apps leave too little room, a device that needs charging is kept first, then the others in their order; with no room for one, the strip hides and the hub still lists every device.
- **Behavior:** click opens or closes the hub; right click shows the tray icon's menu, which can also turn the strip off; hovering lists each device with its level and time left.
- **Contrast themes:** window text, highlight and highlight text system colors instead of white, yellow and ink.

### Window
- **Open:** rises 14px to rest over 260ms (exponential ease-out) while fading in over 160ms; skipped when animation effects are off. Closes instantly on focus loss or Escape.

## Do's and Don'ts

### Do:
- **Do** reserve sign-yellow for the act-now plate, the needing-charge level tag and the tray icon and the strip plate in the same state.
- **Do** keep pictograms white on a dark square (ink on yellow, raised or rule on black), from Segoe Fluent Icons only.
- **Do** set figures with digits at full size and weight and units smaller and lighter (40px/700 against 24px/600).
- **Do** keep plates square and let only Windows-owned silhouettes (frame, tray tile, app icon) round.
- **Do** read every color through the theme brushes so contrast themes remap it.
- **Do** swap instantly when Windows animation effects are off.

### Don't:
- **Don't** use yellow for hover, selection or decoration, or anywhere in the panel or tray when nothing needs charging (the app icon tile is the one standing exception).
- **Don't** add a second signal color for critical; escalate the words instead.
- **Don't** show devices as a grid of equal battery bars or cards.
- **Don't** add drop shadows inside the panel; separate with tone and 1px hairlines.
- **Don't** use all-caps labels or uppercase kickers; copy is sentence case.
- **Don't** use bounce, overshoot or spring easing; motion is mechanical (exponential ease, under 300ms).
