---
name: Lyra
description: A self-hosted mail client. Mail you host yourself.
colors:
  ink: "#1D1B17"
  paper: "#FFFFFF"
  canvas: "#F8F7F4"
  list-mist: "#F7F6F2"
  panel-fog: "#F1EFE9"
  hover-wash: "#E9E6DF"
  hairline: "#E5E2DA"
  button-border: "#E3E0D8"
  secondary-ink: "#6F6A5F"
  tertiary-ash: "#A39D90"
  unread-amber: "#E2A336"
  sync-green: "#3D9A5F"
  destructive-red: "#B4453C"
typography:
  display:
    fontFamily: "'Inter Tight Variable', 'Inter Variable', ui-sans-serif, system-ui, sans-serif"
    fontSize: "20px"
    fontWeight: 500
    lineHeight: 1.25
  title:
    fontFamily: "'Inter Variable', ui-sans-serif, system-ui, sans-serif"
    fontSize: "15px"
    fontWeight: 600
    lineHeight: 1.35
  body:
    fontFamily: "'Inter Variable', ui-sans-serif, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.55
  label:
    fontFamily: "'Inter Variable', ui-sans-serif, system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: 1.3
  brand:
    fontFamily: "'Instrument Serif', ui-serif, Georgia, serif"
    fontWeight: 400
rounded:
  sm: "5px"
  md: "6px"
  lg: "8px"
  xl: "11px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "16px"
  lg: "24px"
  xl: "32px"
components:
  button-primary:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.lg}"
    padding: "8px 16px"
  button-primary-hover:
    backgroundColor: "{colors.hover-wash}"
    textColor: "{colors.ink}"
  input:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.lg}"
    padding: "8px 12px"
  card:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "10px"
    padding: "20px"
  nav-item-active:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.md}"
    padding: "6px 10px"
---

# Design System: Lyra

## 1. Overview

**Creative North Star: "The Dispatch Desk"**

Lyra is a well-kept desk where the day's correspondence is sorted, read, and answered. Everything on it has a place: paper surfaces, hairline rules, a single postage stamp for a logo. Nothing shouts, because a desk you work at all day should disappear into the work.

The palette is warm paper until something needs you. Amber marks the unread, green marks a healthy sync, a muted red marks the rare destructive act. Typography is Inter everywhere, with Instrument Serif permitted exactly twice: the "Lyra" wordmark and the stamp's "L". Depth comes from tonal layering and 1px hairlines, never from shadows at rest.

This system explicitly rejects the stock shadcn mail look, indigo/purple-blue SaaS palettes and sterile institutional cool grays, gradient text, glassmorphism, solid black CTAs, hero-metric dashboards, identical icon-card grids, nested cards, and colored side-stripe borders.

**Key Characteristics:**
- Status-only color on a warm-paper field
- Hairline separation, flat surfaces, no resting shadows
- White pill buttons with hairline borders; never solid fills
- One serif, used twice; Inter for everything else
- Every screen in light + dark, English + Chinese

## 2. Colors: The Status-Only Palette

A warm-paper field where color is reserved for meaning.

### Primary
- **Ink** (#1D1B17): Primary text, the stamp square, chart bars. Ink is for emphasis, not paint: it never fills a button or a banner.

### Secondary
- **Unread Amber** (#E2A336): The unread dot and the "today" bar in charts. Identical in both themes; it is the warmest thing in the product.
- **Sync Green** (#3D9A5F): The sync dot and toggles in the on position. Health, nothing else.
- **Destructive Red** (#B4453C light / #D4756B dark): Destructive text only. The only red anywhere, and it never fills a surface.

### Neutral
- **Paper** (#FFFFFF light / #21201C dark): The reading surface and the message-list column — content lives on paper, rows divide by hairlines. Also cards and active pills.
- **Canvas** (#F8F7F4 light / #12110f dark): The login backdrop; the room the desk sits in.
- **List Mist** (#F7F6F2 light / #171614 dark): The selected-row wash and standalone-nav background. In dark mode this is the app canvas.
- **Panel Fog** (#F1EFE9 light / #1B1A17 dark): Sidebar background and avatar tiles.
- **Hover Wash** (#E9E6DF light / #282622 dark): Hover states, the segmented-control track.
- **Hairline** (#E5E2DA light / #302D28 dark): 1px rules and card borders. The only permitted divider.
- **Button Border** (#E3E0D8 light / #383530 dark): Input and button outlines, one step stronger than Hairline.
- **Secondary Ink** (#6F6A5F light / #9D988D dark): Secondary text, unread counts.
- **Tertiary Ash** (#A39D90 light / #706B60 dark): Placeholders, inactive icons, focus ring.

### Named Rules
**The Status Rule.** Color means status: amber for unread, green for healthy, muted red for destructive. If a color carries no status, it is gray.

**The Constant Status Rule.** Status colors never change between light and dark. The grays invert; the meaning does not.

## 3. Typography

**Display Font:** Inter Tight Variable (falls back to Inter Variable, system-ui)
**Body Font:** Inter Variable (falls back to ui-sans-serif, system-ui)
**Brand Font:** Instrument Serif (falls back to ui-serif, Georgia) — wordmark and stamp only

**Character:** A quiet sans doing all the work, with one serif reserved for the signature. Chinese renders through system fallbacks with equal care to metrics and truncation.

### Hierarchy
- **Display** (Medium 500, 20px, 1.25): Page titles and dashboard KPI numerals, always Inter Tight.
- **Title** (SemiBold 600, 15px, 1.35): Folder titles, card headings, the message subject in the reader.
- **Body** (Regular 400, 14px, 1.55): UI text and the rendered mail body. Line length capped at 65-75ch.
- **Label** (Medium 500, 12px, 1.3): Counts, badges, timestamps, section headers.
- **Brand** (Regular 400): "Lyra" and the stamp "L". Nowhere else.

### Named Rules
**The One Serif Rule.** Instrument Serif appears in the wordmark and the stamp. A third use is a bug.

**The No-Emphasis-Paint Rule.** Hierarchy comes from scale and weight, never from color. Colored text is status text.

## 4. Elevation

Flat by default. Depth is conveyed by tonal layering (Canvas < Panel < List < Paper) and 1px Hairline rules, never by resting shadows. A single faint token, `--shadow-whisper`, exists for the rare floating surface; if a screen needs more than a whisper, the layout is wrong, not the shadow.

### Shadow Vocabulary
- **Whisper** (`0 1px 2px rgba(26,27,31,0.04), 0 0 0 1px rgba(226,226,229,0.8)`; dark: `rgba(0,0,0,0.2)` + hairline ring): Popovers and floating menus only. Never on cards at rest.

### Named Rules
**The Hairline Rule.** Regions divide by a 1px Hairline rect. If it looks like a 2014 app, the shadow is too dark and the blur is too small; if it looks like Lyra, there is no shadow at all.

## 5. Components

### The Stamp (signature)
- **Shape:** Solid Ink square, corner radius 20-25% of its size, with a serif "L" in the surface color. In dark mode the square goes light and the "L" goes dark.
- **Where:** Login card, mail sidebar footer, top of every standalone-page nav. It is the only logo; there is no icon-font substitute.

### Buttons
- **Shape:** Gently curved pills (8px radius), padding 8px 16px.
- **Primary:** Paper fill, 1px Button Border, Ink text. Never solid black, never brand-colored.
- **Hover / Focus:** Hover Wash background; focus ring in Tertiary Ash. 150-220ms ease-out.
- **Ghost / icon:** No border, Tertiary Ash icon at rest, Ink on hover. Icon buttons in toolbars sit on hairline-separated groups.

### Inputs
- **Style:** Paper fill, 1px Button Border, 8px radius, Tertiary Ash placeholder, 8px 12px padding.
- **Focus:** Border strengthens toward Tertiary Ash; no glow, no color shift.
- **Error:** Destructive Red text and border, message in sentence case below the field.

### Cards
- **Corner Style:** 10px radius.
- **Background:** Paper.
- **Shadow Strategy:** None at rest (see The Hairline Rule).
- **Border:** 1px Hairline.
- **Internal Padding:** 20px, tightened to 16px in dense lists.

### Toggles
- **Style:** 36x20 pill; Sync Green when on, neutral gray when off; white knob. The state reads from across the room, which is the point.

### Segmented Control
- **Style:** Hover Wash track, Paper active pill, 6-8px radius. Used for All/Unread and range pickers (7/30/90 days).

### Navigation
- **App rail (52px, Panel Fog):** Far-left on the mail screen — stamp, Mail / Contacts / Calendar (calendar carries today's date badge), then Dashboard / Settings / theme at the bottom. Standalone pages keep the slim nav instead.
- **Mail sidebar (240px, Panel Fog):** Account pill (Paper + 1px Button Border) and compose on top; UNIFIED and ACCOUNTS sections; folder tree with 16px indent per level, disclosure chevrons, thin-stroke role icons, and a pastel tone dot for custom folders. Selected row is a Paper pill with a 1px Button Border. Footer: wordmark + sync dot only.
- **Standalone slim nav (220px, List Mist):** Stamp + wordmark, "← Mail" return, section list. Dashboard and Settings share this shell exactly.

### Correspondence tones (avatar exception)
- Sender avatars and custom-folder dots use 12 deterministic muted pastels (`avatar-tone-0…11`), hashed from the sender/folder name. This is the only decorative color in the product; saturation stays low so status colors still win.

## 6. Do's and Don'ts

### Do:
- **Do** reserve color for status: Unread Amber #E2A336, Sync Green #3D9A5F, Destructive Red #B4453C, everything else warm paper gray (The Status Rule).
- **Do** separate regions with 1px Hairline #E5E2DA rules; keep surfaces flat (The Hairline Rule).
- **Do** build primary actions as white pills: Paper fill, 1px Button Border, Ink text, 8px radius.
- **Do** render every screen in light + dark and en + zh before calling it done; status colors stay constant across themes (The Constant Status Rule).
- **Do** keep the stamp square's corner radius at 20-25% of its size and invert it by theme.
- **Do** respect `prefers-reduced-motion`; transitions run 150-300ms on ease-out curves, transform and opacity only.

### Don't:
- **Don't** ship the stock shadcn mail look or the old indigo accent; both were reviewed and rejected.
- **Don't** use indigo/purple-blue SaaS palettes, gradient text, or glassmorphism anywhere.
- **Don't** fill a button or banner with solid black or a brand color; Ink is for text, not paint.
- **Don't** build hero-metric dashboards (big number, small label, gradient accent) or identical icon-card grids.
- **Don't** nest cards, add resting drop shadows, or use border-left/right greater than 1px as a colored stripe.
- **Don't** use Instrument Serif anywhere but the wordmark and the stamp (The One Serif Rule).
- **Don't** surface protocol jargon (IMAP/JMAP/OAuth) in the reading flow; it belongs in Settings.
