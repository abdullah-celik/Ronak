# Vision: from windows to what you need

Today's desktop is built from *apps* drawn in *windows*. You do the work of
fitting them to your task: opening, switching, resizing, hunting. Ronak's
long-term aim is a desktop that is composed **for the task you are doing**,
showing only the pieces you need, arranged for you, and using no more of
your screen, attention or battery than that.

Others are approaching the same idea from different directions: generative
UI inside chat assistants, "malleable software" research, intent-driven OS
concepts like Mercury OS, and agent-first devices like Microsoft's Project
Solara. Ronak's place is the bridge almost nobody is building: **an open,
local, OS-level layer that works with the apps you already have, on the
computer you already own**, and gradually makes windows a fallback instead
of the main unit.

## Four stages

| Stage | Unit | What Ronak does | Status |
| --- | --- | --- | --- |
| 1. Arrange | the window | Infers your task, arranges whole windows for it | v0.1 |
| 2. Understand inside | the element | Reads inside apps (accessibility tree), recognizes finer intents, explains itself, learns from corrections | v0.2 |
| 3. Compose | the capability | Pulls data and actions from apps (media, calendar, notifications, MCP tools) into surfaces Ronak draws itself | v0.2 starts it |
| 4. Intent-native shell | the surface | Your workspace *is* a generated surface for the task; apps are capability providers; windows only for software that cannot be decomposed; idle apps are paused to save resources | later |

Each stage reuses the previous one. Roles (primary, reference, monitor,
background) generalize from windows to elements and capabilities; the layout
solver grows into a surface composer; the action registry grows into app
capabilities; the context schema grows by addition.

## Principles that do not change

1. **The AI decides what matters; a predictable system decides how it
   looks.** Models may compose surfaces freely, but only from a catalog of
   trusted components, and every surface is validated against a schema
   before anything is drawn. If validation fails, the deterministic composer
   takes over.
2. **Local first.** Data is born and stays on the device. What leaves, if
   anything, is the user's decision, per consumer, per detail level.
3. **Explain and accept.** Ronak shows why it thinks what it thinks, and
   changes the screen only when you accept (or after you opt into automatic
   mode). Every change is undoable.
4. **Corrections are the training signal.** Each time you correct Ronak, it
   learns, first as labels, later as a model on your own NPU.
5. **Open contracts.** The context schema, adapter protocol and surface
   catalog are public specifications, so others can build adapters,
   composers, and renderers in any language and on any OS.

## v0.2: understanding intent

v0.2 is built around one question: *how well does Ronak understand what you
are doing?* It adds:

- **Elements**: structure and content of the focused app, through AT-SPI.
- **Context capabilities**: what is playing, what is on your calendar,
  which notifications arrived, which MCP tools you have.
- **Finer intents**: a task (coding) plus an activity (debugging, reviewing,
  testing) and a project, with the evidence behind them.
- **The intent panel**: shows what Ronak believes and why, and lets you
  correct it with one click.
- **Context glances**: small cards for music, the next meeting and recent
  notifications, so those apps do not need a window.
- **Gentler resource use**: apps outside the current task run at lower CPU
  priority.

## What would make this succeed

- **Trust.** A layer that reads inside every app is the most sensitive
  software on a computer. Privacy has to be structural, not a setting.
- **Stability.** Generated interfaces that change every time destroy muscle
  memory. Layouts and surfaces must be stable unless something meaningful
  changed.
- **Capabilities over pixels.** Decomposing apps by scraping their pixels is
  fragile. The durable path is apps exposing data and actions (D-Bus
  standards, MCP servers) and Ronak generating interfaces over them.
- **Open source as proving ground.** Linux apps and standards are the place
  to show the idea works before asking any platform to adopt it.
