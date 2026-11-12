# Release notes

## 0.4.0 — Key maps that follow the scene

### Added

- **Advanced mapping and scene detection.** A key map can have separate keys for **on foot**, **in a vehicle** and **in a menu**, and console buttons can carry an action (`key_enter_or_exit_vehicle`, `key_change_camera`, `key_pause_game`). A single **Advanced mapping** switch in the key editor (it works during a stream) turns on the layers and lets two on-device vision models (MobileCLIP-S1 and DINOv2-B through Core ML) read the whole picture about twice a second and pick the layer, with the actions steering it. See `docs/USAGE.md`.
- **Key mode box.** With advanced mapping on, a small box in the top-right corner of the stream shows the key mode as an icon (on foot, in a vehicle, in a menu) instead of a message at every change of scene.
- **Layers in use.** Each key map can switch off the on-foot, in-vehicle or in-menu layer for a game that has no use for it (`disabled_layers`); a switched-off layer is never used and its scene is never picked, so a game without vehicles cannot start the vehicle keys.
- **Training tools.** `tools/scene-context/` is one command now (`scene.py`) with two scripts for everyday use: `label.sh` extracts new captures from `~/openps-captures` and serves the labeling page (every recording, labeled ones too, one set at a time; each change is saved at once into `dataset/labels.json`, and Del drops a frame), `train.sh` computes features only for new frames, converts the models to Core ML only when they changed, writes `assets/models` and the files the Rust tests compare against, and prints how recordings the model has not seen are read. The labeled frames are also sorted into `dataset/sorted/` (first_person, third_person, on_foot, in_vehicle, in_menu). The experiments behind the design are in `tools/scene-context/experiments/`, their results in `results/`. The trained models (about 200 MB) are no longer tracked by git.
- **Longer video captures.** `OPENPS_VIDEO_CAPTURE_SECONDS` and `OPENPS_VIDEO_CAPTURE_MB` set the capture limits (default 90 seconds or 200 MB). `scripts/record-session.sh` now records for up to 30 minutes or 4 GB by default.

### Changed

- **App installation.** The installer includes trained scene models when available and signs the app with the fixed bundle identifier `app.openps.client`. It uses ad-hoc signing by default; set `OPENPS_SIGN_IDENTITY` to use another signing identity.

## 0.3.0 — Controller feel, video recovery and a tidier interface

This release finishes the controller work started in 0.2.0 (haptics, adaptive triggers, rumble), makes video recover better from
packet loss, and reworks the settings and stream window.

### Added

- **Better upscaling quality (MetalFX).** The upscaler now runs in linear light, and a light adaptive sharpening pass (AMD's RCAS, ported
  and checked against AMD's reference code) follows the sharpness blend, strongest at the default 60 % and absent at 0 % and at 100 % or
  more. Measured against real footage (clean, and encoded at 2.5 to 8.4 Mbit/s at 540p) the picture is closer to the truth than before at every
  bitrate, for about 0.5 ms of GPU time at 1440p and 2 ms at 4K (and it switches itself off if the GPU is close to the time there is). The measurements, and the things that did _not_ help (denoising first, stronger sharpening, MetalFX's
  temporal mode, heavier machine-learning models, none of which is real time on a compressed stream), are in `tasks/enhance-compare/`.
- **Adaptive sharpening slider** (Settings → Display). The RCAS sharpening that follows the sharpness blend is now a slider of its own (0 to 200 %, default 100 %, needs upscaling) instead of being tied to the sharpness setting. The guard that switches the finishing pass off when the GPU is too busy (now only when Adaptive frame rate is off) is explained in `docs/USAGE.md`.
- **Debug bar.** With Debug tools on, a panel at the left of the stream lists everything running in the backend and whether it is on, off, degraded or not available: renderer, upscaling and frame generation and which engine does them, the finishing pass, the adaptive frame rate and its guard, timing and GPU time, the stream, sound and controllers. It can be folded and copied as text.
- **Immersion mode.** In a window wider than the picture, the black bars are filled with one soft glow of the picture's
  colours (the picture, stretched over the window and blurred, like the ambient light behind a video), so the frame seems to
  continue past its borders without repeating itself. It is off by default. Turn it on for good in
  **Settings → Display → Immersion**, or switch it during a stream with the **Immersion** button in the stream bar. It needs the
  Metal renderer, and nothing changes when the window has no bars.
- **Every picture filter has its own switch** (Settings → Display): Linear light, Sharpness, Adaptive sharpening, Saturation and Frame
  generation, next to the existing Upscaling (Off), Camera latency compensation and Immersion. A switch keeps its slider's value, so a filter
  can be turned off and on again.
- **Shimmering on repeating vertical lines while the camera moves (frame generation) is fixed** (reported with a recording of a
  radiator). The motion search matched a line to the next one over, differently from block to block and frame to frame. It now prefers
  the shorter vector and the one nearest to the block's last one, uses the motion of the whole picture when the rest of the picture
  settles it (fast pans beyond the old 16 px range work), refines to quarter pixels, and resamples the generated frame with a sharp
  filter (the bilinear one softened thin lines in generated frames only, so they flickered against the real ones). Tests: a pan over
  repeating lines at 4 spacings and speeds from 0.6 to 26 px a frame. A pan of more than half the line spacing a frame with nothing else
  in the picture to say how far cannot be generated right by any method; see `docs/USAGE.md`.
- **Shimmer on the radiator, found in a recording of a real session.** The first fix was measured on a synthetic scene and missed the real
  one: the radiator's fins repeat every 12 pixels and move 5, no block inside them is clear, and a patch of blocks took the vector a
  spacing away and showed the fins in the wrong place. Blocks now know whether they are ambiguous, and ambiguous ones take the vector of
  the clear blocks next to them; fine blocks follow the neighbourhood's median; the neighbours' vectors are tried only where a block's own
  samples disagree and are judged over a window. On the recording: error on thin vertical structure 11.5 → 10.2 levels, on the radiator
  4.5 → 2.8, and the patch is gone. `scripts/record-session.sh` records a session, `tests/replay.rs` replays it.
- **Shimmer that grows with the generated frame rate: found and fixed.** Replaying the pictures a display shows in a row (60 fps stream,
  144 ticks a second) showed generated pictures with 88 to 93 % of the detail of real ones (91 % of the grain) and the detail changing
  5.9 % from one picture to the next (1.6 % between real ones): a generated picture was the average of two real pictures, which loses
  detail and halves the grain, and the resampling softened it; real pictures are full strength, so detail pulsed at the beat of the
  generated frames, more the more of them there were. A generated picture now comes from the nearer real picture alone, moved into place,
  with a slightly sharper filter: 97.6 to 103 % of the real detail at 144 and 99.6 to 105 % at 240, 3.75 % and 4.3 % picture to picture.
  The price is a little accuracy against the exact picture on grainy footage (12.1 → 13.2 levels): it is as grainy as the real ones.
  The motion vectors were checked against exact ground truth for a pan, moving forward, a roll and a pan with depth: within 1 px.
- **Side-to-side camera motion now uses the controller (or the mouse) as well as the picture.** The app already knows what the stick asked
  for; it learns how the game answers it (the delay, and how many pixels of picture per unit of stick, from the motion it measures: it
  needs a few seconds of camera movement, and only works if the stick explains at least 60 % of that motion) and hands the motion search
  the camera motion the stick commanded between the two frames. The search looks around that vector too, and among matches that are
  equally good it prefers the one nearest to it (the less it is trusted, the weaker the preference), so a row of look-alike lines (a
  radiator, a fence) or a plain wall with nothing to match no longer decides the vector by chance; blocks that no neighbour can settle
  take it as well when the picture has no consensus of its own. A wrong or stale guess does little harm: the picture still decides when it
  is clear. Learning now runs whenever frame generation does (it no longer needs _Camera compensation_, which stays off by default).
  Mouse-look is covered through the keyboard/mouse mapper's right stick (`mouse.look = RIGHT_STICK`, the default; `LEFT_STICK` is not
  seen). **Fixed on the way:** the keyboard/mouse engine used to record its stick even while a physical pad was in control, mixing idle
  zeros into the pad's history; it now records only what reaches the console.
  Synthetic checks (`tests/periodic.rs`: a plain wall and a radiator; a flick of 19 px over fins 22 px apart, and 11 px over 14 px): the
  picture alone is 85 and 87 levels off (as bad as the nearer real picture, 82 and 79), with the stick's motion 1.6 and 1.7, also when the
  stick's guess is 5 % off; a prior that points the wrong way or says "still" costs at most 0.7 levels on a pan the picture can tell (1.6
  to 2.3 against 1.6). A flick of exactly one whole spacing improves (83 → 20) but is not exact. **Not measured on a real game:** the
  recording of a session holds pictures, not the stick, so this cannot be replayed; the Debug tab's _Input motion prior_ row says whether
  it is in use. Sideways pans above about 6 px a frame (roughly 10 % of the pairs in the recording) are still worse than the nearer real
  picture.
- **The 20 to 30 ms gaps in frame pacing are explained and have a switch.** In Smooth mode a real frame that arrives later than the best
  case the network has shown leaves nothing to interpolate toward, so the picture holds for as long as it is late. On a recording of a
  real session the console's frames arrived up to 18 to 30 ms apart at times; replayed at 144 Hz, Smooth mode held the picture 388 times
  in 29 s (12 of 20 ms or more). **Settings → Display → Smoothing buffer** (on by default) holds the picture back by how late the slowest
  2 % of recent frames were (at most 25 ms, 17 ms on that recording): 18 holds, longest 6.9 ms. It costs that latency, in Smooth mode only.
- **Run above the display's refresh rate** (**Settings → Display**, off by default): for low-refresh monitors. Frames are generated and
  handed over at up to 240 a second instead of stopping at the refresh rate; the screen shows the newest at each refresh, which lowers the
  delay (it does not show more pictures than the panel can). Windowed only; costs GPU, and the adaptive frame rate lowers it when needed.
- **The frame rate no longer lurches.** The adaptive frame rate judged every picture alone, so a Mac whose real pictures were too slow while
  its generated ones were quick was never slowed down (each quick one cancelled the count) and showed late frames; when it did drop it jumped
  straight to the new rate and climbed back one step every few seconds, only to drop again. It now looks at the last second of GPU time (the
  mean and the slowest 5 %), comes down only after 2 s of overload, by at most 10 fps at a time and 1.5 s apart, goes up one step of 5 only
  after 5 s of comfort and never within 10 s of a drop. Measured on an M4 at 60 → 144 with 1080p upscaled to 1620p: a real picture costs
  about 5.5 ms and a generated one 3.3 ms (61 % busy at 144 fps), so it holds 144 without ever stepping.
- **Shimmer and tearing in camera pans (frame generation), second round, from a real recording.** The motion search now compares dark
  pictures in a square-root scale (a game in the dark was being judged mostly as flat or still), prefers shorter vectors less (the clear
  blocks around a pattern settle it), no longer calls a block 'static' because its picture is flat, fills blocks with nothing to match from
  what the clear blocks agree on (the camera), and blends the two real pictures where no motion explains a pixel instead of showing the
  nearer one (soft ghost instead of an edge that jumps). On the recording (every second picture fed in, the one between generated and
  compared with the real one): mean error on thin vertical structure 13.3 → 12.1 levels, pixels more than 24 levels off 17.9 % → 15.1 %
  (the nearer real picture: 20.7 and 33.6). The same recording re-encoded at 3 Mbit/s and at 40 Mbit/s: 12.2 and 12.1 levels, 15.2 % and
  15.3 % (the nearer real picture: 20.9 and 20.7), so the bitrate does not change how well frames are generated.
- **Top bar:** every item keeps its own width; the fps item shows the game's rate with its 10 s average, a new item shows the frame time
  with its 10 s average, and the frame generation item shows the rate on screen. The adaptive frame rate moves in steps of 5 fps (was 15) and a fixed-refresh display keeps a rate within a tenth of an even one (55 on 60 Hz) instead of dropping to 30.
- **Frame generation has no rate setting.** It is a switch: on, frames are made to fill the display's refresh rate (up to 240), and the
  adaptive frame rate below lowers it when the Mac cannot keep up. The old multiples (1.5× to 3×), and the target-rate slider of the
  first draft of this change, are gone; a saved one becomes the switch.
- **Adaptive frame rate** (on by default). When a Mac cannot keep every filter you have on running at the rate you chose, the frame rate
  is lowered in steps of 5 fps, to the closest multiple of 5 it can hold, and the picture stays as set: Macs with room to spare show the
  full rate, weaker ones the same picture more slowly, instead of a filter being switched off. It steps back up when there is room. The
  stream bar shows the held rate and the Debug bar says why. Off: the old behaviour, where the upscaler's finishing pass is given up
  instead. `OPENPS_EXTRA_GPU_MS=14` adds that much to every picture's measured GPU time to see it work.
- **FSR as the second engine, replacing VideoToolbox.** **Settings → Display → Engine** is now MetalFX (the default) or FSR: AMD's FidelityFX
  Super Resolution 1 (EASU upscaling, ported to Metal from AMD's MIT-licensed reference and checked against a translation of it), followed
  by the same blend and RCAS sharpening as MetalFX. A saved VideoToolbox engine opens as MetalFX. VideoToolbox's frame processors
  (upscaling and frame-rate conversion) are gone: at 1080p they took 15 to 16 ms a call and had to hand over to MetalFX, and their frame
  conversion never fitted a 60 fps stream. VideoToolbox is still what decodes the video. **FSR 3 and FSR 4 are not offered**: AMD's SDK has no
  Metal backend, and both take what a game engine hands them (depth buffer, motion vectors, camera jitter), which a video stream does not
  carry. FSR only upscales: MetalFX makes the generated midpoints at about 2× with either engine, and the built-in interpolator the other
  rates. **Frame pacing:** on a fixed-refresh display a held rate below the stream's now rounds down to a whole number of refreshes per
  picture (45 → 30 on 60 Hz; it measured ±8 ms and a 37 ms longest gap before, ±0.6 ms after), and the on-screen "shown" statistics no
  longer read 0 when Metal gives no presentation time. The reasons, and what a third-party Metal port of FSR 4
  would and would not change, are in `docs/USAGE.md`.
- **Debug tools** (off by default, **Settings → Display → Debug tools**): an **Engine** button at the left end of the stream bar opens a
  panel that switches the engine and the upscaling resolution on the running stream, and says what is really doing
  the upscaling and the generated frames.
- **Upscaling goes to the size you chose, whatever the screen is.** 1080p at 2× is now made at 4K even on a 1440p screen and then scaled
  down to fit the window (it used to stop at the window's size), which usually looks better. It costs more GPU time the larger it is.
- The **Leave** button now sits right beside the centred stream bar (above it, at the right, in a very narrow window).
- **Settings → Controls.** Rumble strength (0–200 %) and adaptive-trigger strength (0–100 %) have −/+ buttons, an icon Test button
  and a "?" with the explanation. A Test button is disabled until its test has finished, so two tests cannot overlap.
- **Settings and stream window.**
  - Upscaling is a multiple of the stream's resolution (1.33×, 1.5×, 2×) with a badge on each option that shows the
    resulting size, and the stream's top bar shows the size in use next to the upscale icon.
  - "Interpolation engine" is now "Frame Gen" (no longer marked experimental) and "Extra frames" is "Frame Gen Latency".
  - The frame diagnostic is one small row with a "?" for its explanation.
  - The Controls tile shows a mouse instead of the controller while keyboard and mouse control is on and keeps its name; a message
    at the top says when it is switched on or off, and every message now fades out.
  - The key editor opens above the stats without moving them, its profile tabs wrap instead of being hidden, and the stats pin
    turns when pinned.
  - The adaptive-trigger Test button has a trigger icon instead of a download arrow.
  - Every page was checked from phone width (320 px) to wide windows; the shortcut list and the controller card fit small screens.
- **Controller battery.** The Consoles page shows each connected controller's battery as a bar whose coloured fill follows the
  level: red up to 25 %, yellow up to 50 %, green above. It disappears when the controller is unplugged.
- The Consoles page refreshes your games and trophies when a stream ends and every 5 minutes.
- `scripts/install.sh` starts a fresh install with the key map in `./openps-data/` (`input.toml`, `game-keymaps.json`) and never
  overwrites an existing one.
- A second touchpad finger is sent to the console, with its own touch id.
- Adaptive-trigger intensity follows the console setting (Medium, Weak, Off).
- **Bluetooth haptics follow the original app's format and pacing.** Frames are queued and sent as 64-byte blocks at the rate
  they play (two blocks per report), the pad's haptic coils are switched on first, the report counter is in the right place, and all Bluetooth output
  reports share one sequence number. Console rumble is held back while haptics play, and the coils are told to stop when the
  haptics or the session end. Previously each frame was sent on arrival with a misplaced counter and the coils were never set up.
- The DualSense's firmware version is read, and rumble is written the way that firmware expects (newer firmware with its
  second vibration flag and full strength, older firmware with the compatibility flag and halved values).
- The console's "reset motion" request is honoured: the orientation restarts from the current heading. The orientation filter
  also converges quickly when a pad is held tilted at the start, using a short high-gain warm-up.
- A DualSense over USB now rumbles with the game's haptic audio (the rumble stops within 150 ms of the audio ending).
- Returning to the app re-reads the controllers, so a button released while it was in the background cannot stay held.
- A session that connects but never shows a picture now fails with a clear message after 15 seconds (a warning is logged
  after 2) instead of staying "connected" on a black screen. If the console was streaming HEVC or HDR, OpenPS then
  retries once with H.264.
- Opt-in video capture for bug reports (`OPENPS_VIDEO_CAPTURE=<directory>`): records the frames the console sent, with ids and
  arrival times, for offline study of loss recovery. See `docs/USAGE.md`. A lost video frame is now noted in the log.

### Fixed

- **The adaptive-trigger and rumble tests never ended.** A DualSense keeps the last effect until it is told otherwise, and a report
  without rumble or trigger data says nothing, so a Test effect kept going unless the console had asked for one earlier. A test now
  ends by writing an explicit stop.
- The controller battery bar showed nothing in its fill; it now draws the fill and colours it by level. (The page's content security
  policy blocks inline styles, which is why the first version of the fill did not appear.)
- Upscaling set to "Off" still upscaled to the whole window; Off now means off.
- **Video recovery after packet loss.** The video reassembly window is 2 frames, the original app's default (it was 3),
  so a lost frame costs the next frame its reference less often.
- **Packet loss that error correction repaired is now counted.** Automatic bitrate saw almost no loss on a lossy link
  because the repaired packets were not counted; they are now reported to the console in the congestion packet.
- If the video decoder fails three times in a row without showing a picture, its VideoToolbox session is rebuilt instead of
  asking for keyframes it can no longer use.
- Congestion reports are sent to the console only while the bitrate is Automatic, as the original app does.
- The session's uptime is no longer cleared after the first second of statistics, which also made the "retry with the
  default bitrate when the console ends the session early" safeguard work as intended.
- A controller that cannot be opened for feedback is retried once a second instead of on every write, which had flooded
  the log and used CPU.

### Known limitations

- The adaptive-trigger intensity, the Test buttons' feel and USB haptic-to-rumble behavior have not been verified on hardware.
- Bluetooth voice-coil haptics, the newer rumble flags and the motion reset have been tested only up to the point where the pad
  accepts every report (on a DualSense with firmware 0x0630); how they feel in a game has not been checked.
- FSR's upscaling has been checked pixel by pixel against a translation of AMD's reference and run in the whole renderer, but its look in a real stream has not been judged by eye. The adaptive frame rate has been tested with artificial GPU load (and in the real window that way); it has not been seen on a Mac that is really too slow.
- True voice-coil haptics over USB are not implemented, nor is output (rumble, lightbar) for a DualShock 4.
- With several DualSense controllers, which one gets which feedback follows the order they connected: macOS gives no serial
  number to match them by.

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
- Vibration strength follows the console setting: Strong is full strength, Medium half, Weak about a third, Off none.
  Until the console reports a setting the controller vibrates at full strength.

### Known limitations

- The adaptive-trigger intensity setting and a second touch finger are not sent to the console yet.
- Bluetooth voice-coil haptics have not been verified on hardware; USB haptics are not implemented.
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
