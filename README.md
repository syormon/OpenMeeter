# OpenMeeter

A Voicemeeter-style audio mixer for Windows and Linux. Windows requires `VB-Cable`/`VAC` and Linux requires `PipeWire`/`PulseAudio`

## Status

- [x] Windows device enumeration, VB-Cable detection
- [x] Windows mixing engine: capture → mix → render, gain/mute/mono/solo, routing, real meters, feedback-loop guard
- [x] Recorder: arm inputs (pre-fader) or buses (post-fader), WAV 16/24/32f, mono/stereo, multitrack,
      auto-stop; play WAV/MP3/FLAC/OGG/M4A into buses with gain, loop and play-on-load
- [x] Menu: restart / auto-restart engine, presets (load, save, load on startup), reset, system tray,
      run on startup, start hidden, always on top, lock UI, system settings (buffer size, strip names)
- [x] Windows: per-output exclusive mode (bypasses the Windows mixer and effects, like Voicemeeter)
- [x] Windows: outputs play in raw mode, bypassing device effects (Realtek "enhancements", virtual surround, loudness) without taking the device exclusively
- [x] Macro buttons: global hotkeys (e.g. Ctrl+F12, Numpad7) for the recorder transport (play/pause, stop, rewind, fast forward, record, play a sound clip, eject), strip and bus controls (mute, solo, mono, A1/B1... outputs, set or nudge a fader), loading a settings file, showing/hiding the window and restarting the engine (X11 only on Linux)
- [x] Macro scripts in Lua 5.3, using Voicemeeter's Remote API parameter names (`Strip[0].Mute = 1`)
- [x] Custom colour themes from the config file (see [Theming](#theming))
- [x] Adjustable mixer layout: 1-8 hardware inputs, 0-4 virtual inputs, 1-5 hardware outputs, 0-3 virtual outputs (Menu > Mixer Layout; 3 / 2 / 3 / 2 by default)
- [ ] MIDI mapping, VBAN
- [x] Linux backend: device enumeration, virtual inputs/buses created on demand, mixing engine,
      meters, recorder and player, feedback guard, unplug detection, real-time audio threads, tray icon
- [ ] EQ/comp/gate/pan DSP
- [ ] Per-app routing

## Linux Quickstart

![openmeeter](./images/openmeeter-linux.png)

On Linux, virtual devices appear in every app's device list while OpenMeeter runs:
play into **OpenMeeter Input v1** (e.g. set it as your default output) and record
from **OpenMeeter Out B1** (e.g. as Discord's microphone). They are removed on exit.

## Windows 10+ Quickstart

![openmeeter](./images/openmeeter-windows.png)

You will need some virtual audio drivers. If you're looking for a free version:

[Cable 1 (VB Cable)](https://vb-audio.com/Cable/)

[Cable 2 (VAC)](https://vac.muzychenko.net/en/download.htm)

> If you're wanting more than 2 audio channels, you can pay a small fee for either. Signing audio drivers on Windows is actually mad expensive, which is why it is not possible to create a free/FOSS version.

Install both cables. They should show as sound drivers like so:

![VAC](./images/vac-driver.png)
![VB-Cable](./images/vbcable-driver.png)

Make one of them the default for `Playback`, and one default for `Recording`.

> Don't forget to tinker with `Properties` > `Advanced` to make sure you have the right sample rate!

In `OpenMeeter`:

1.) First select your audio hardware. You can select your output devices in the top right in `HARDWARE OUT`, where each devices takes a channel (A1-A3). For your input devices, Select your driver for `HARDWARE INPUT`.

2.) For `Virtual Inputs`, Select `CABLE Input`. In the screenshot above, you'll see its selected for the first Virtual Input.

3.) For `Master Section`, select one of the last second columns (B1,B2) and select `LINE 1`.

4.) Make sure your devices are on the right channels. In the screenshot, I have the `CABLE Input` outputting to `A1`(speakers) and I have `Focusrite USB Audio Mic` outputting to `B1`, which is `LINE 1`

## Theming

Every colour can be overridden from the config file like as so:

```json
{
  "mixer": { ... },
  "settings": {
    "theme": {
      "bg": "#1d1b26",
      "accent": "#b48ef0",
      "boost": "#27F561",
      "fader_track": "#D113B7",
      "meter_green": "#8a6cf0"
    }
  }
}
```

`bg`, `accent` (lit buttons, faders, knob rings) and `boost` (a fader above 0 dB) each recolour
their whole family in matching shades; `openmeeter theme` lists every colour that can be set.

> **`Menu` > `Load Settings...`**

## Macro scripts

For anything the built-in macro actions don't cover, a macro can run a Lua 5.3
script. Write scripts in **Menu > Macro Scripts (Lua)** (they're `.lua` files in a
`scripts` folder beside the settings; `openmeeter config-path` shows where), then
bind one to a hotkey with the **Run Script** action in **Menu > Macro Buttons**.

Scripts use the parameter names of Voicemeeter's Remote API, so most Voicemeeter
macro-button scripts carry over. Strips and buses count from 0: hardware inputs,
then virtual inputs; A1, A2..., then B1, B2...

```lua
Strip[0].Mute = 1 - Strip[0].Mute        -- toggle the first input's mute
Strip[0].B1 = 1                          -- send it to B1
Bus[0].FadeTo = "(-10, 500)"             -- fade A1 to -10 dB over half a second
Recorder.load = "C:/clips/airhorn.wav"
Recorder.play = 1
SetParameters("Strip[1].Gain += 3; Strip[1].A1 += 1")   -- Voicemeeter's own syntax
```

The full list is under **API reference** in the Macro Scripts window. Scripts can
`require` other scripts in the folder, `print` to the window's output, and keep
globals between runs. They can't touch files, programs or the network, and a run
is stopped after 250 ms.

## Running locally

```
cargo run                      # GUI with the platform backend
cargo run -- --mock            # GUI with a fake backend (any OS)
cargo run -- devices           # list audio devices
cargo run -- doctor            # check for problems (e.g. VB-Cable missing)
cargo run -- meters            # run the mixer headless and print meter levels
cargo run -- record --seconds 30 [--play song.mp3]   # headless recording
```

Ubuntu/Debian build deps: 
```bash
sudo apt install libpulse-dev libgtk-3-dev libayatana-appindicator3-dev pkg-config
```

> backend needs `pactl` (`pulseaudio-utils`), which comes with PipeWire, normally
Audio threads ask for real-time scheduling (directly, or through RealtimeKit); `meters` shows what they got.

With the system tray on, the window runs through XWayland on Wayland desktops: Wayland doesn't let
apps hide their windows, which closing to the tray needs.
