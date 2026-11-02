# Release notes

## 0.2.0 — Physical controllers

This release adds support for a physical DualSense or DualShock 4, and fixes how controller input is sent to the console.
The controller path has been run against a real PS5.

### Added

- **Physical controller support.** A connected DualSense or DualShock 4 (through macOS GameController) controls the game
  by default, up to four at once. **Controls** in the stream bar, or **F10**, switches between the controller and
  keyboard and mouse, and each switch releases the previous source. **Settings → Controls** shows each controller's
  type, slot and battery and sets the stick deadzone.
- **Console feedback on the controller.** Rumble, lightbar colour and adaptive-trigger effects are forwarded to a
  DualSense, and the console's vibration-strength setting is followed. Bluetooth voice-coil haptics are experimental.
- **Input tracing for bug reports.** Start the app from a terminal with `OPENPS_INPUT_TRACE=1` to log every pad change at
  each stage (what macOS reported, what was forwarded, the packet that was built, and whether the socket accepted it)
  with a microsecond clock. See "Controller input diagnostics" in `docs/USAGE.md`. It is off by default; while it is on,
  the log file is not rotated.
- A warning in the log when the network refuses to send a packet (previously silent).
- A second touchpad finger is sent to the console, with its own touch id.
- Adaptive-trigger intensity follows the console setting (Medium, Weak, Off).
- A DualSense over USB now rumbles with the game's haptic audio (the rumble stops within 150 ms of the audio ending).
- Returning to the app re-reads the controllers, so a button released while it was in the background cannot stay held.
- `scripts/install.sh` prints the trace launch command, and `scripts/make-dmg.sh` builds a disk image.

### Fixed

- **Pressing or releasing two controls at once could be misread by the console.** The console applies only one new input
  event per feedback packet, but a change that moved several controls (typically LT and RT together) was sent as a single
  packet. One trigger was then applied late or not at all: a car in a racing game would move slowly, burn out, or
  reverse instead of doing one clear thing. Each control change is now sent in its own packet, oldest first, as the
  original app and chiaki-ng do. Confirmed on a PS5 with LT+RT presses and releases. The same encoder serves keyboard
  and mouse mapping, so it applies there too.
- Touchpad clicks are ordered with their finger: button releases go first, then touch, then button presses, then
  triggers.
- A controller joining mid-session is announced to the console for about 2.3 s instead of half a second, so one lost
  packet no longer leaves it ignored.
- Unplugging a controller, losing focus, or switching to the keyboard resets its motion sensors as well as its sticks
  and buttons, so the console no longer keeps seeing the last tilt.
- A controller that cannot be opened for feedback is retried once a second instead of on every write, which had flooded
  the log and used CPU.
- Vibration strength follows the console setting: Strong is full strength, Medium half, Weak about a third, Off none.
  Until the console reports a setting the controller vibrates at full strength.

### Known limitations

- The adaptive-trigger intensity and USB haptic-to-rumble behavior have not been verified on hardware.
- Bluetooth voice-coil haptics have not been verified on hardware; true voice-coil haptics over USB are not implemented.
- A DualShock 4 gets input but no rumble or lightbar output.
- PSN-based internet registration is experimental and has **not been tested against Sony's servers**.

## 0.1.0 — Initial pre-release

OpenPS is a native macOS client for PlayStation Remote Play, built with Rust and
Tauri. This release establishes the complete application and streaming stack.

### Added

- Console discovery, pairing, wake, connect, and session controls for PS4 and
  PS5.
- H.264 and HEVC video handling with VideoToolbox decoding and Metal rendering,
  including optional MetalFX upscaling and frame generation.
- Opus audio playback, haptic feedback, and stream recovery and feedback
  handling.
- Keyboard-and-mouse controls with editable key maps and per-game profiles.
- PlayStation Network sign-in, console game library and trophy views, and
  experimental PSN-based PS5 registration.
- A built-in demo console and mock consoles for trying the application and
  running tests without a PlayStation.
- Local application data storage and build, install, demo, and uninstall
  scripts.

### Platform

- macOS 13 or later on Apple silicon or Intel.

