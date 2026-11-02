# OpenPS usage guide

## 1. Before you start

On the console:

1. Turn it on, or put it in **rest mode** (to wake it from the app, enable *Settings → System → Power Saving → Features Available in Rest Mode → Stay connected to the internet* and *Enable turning on PS5 from network*).
2. **Settings → System → Remote Play → Enable Remote Play**.
3. Keep the console and your Mac on the same network. Wired Ethernet on the console gives the lowest latency; Wi-Fi 5 GHz is fine.

On the Mac, build and open the app (`scripts/install.sh`, then `open install/OpenPS.app`). The first time macOS may ask to allow local network access: allow it, otherwise the app cannot see the console.

## 2. Pair a console (once per console)

1. **Consoles → Add console** (or **Add your console** on the first-run guide).
2. Choose your console from the list (or type its IP address) and its type, then **Continue**.
3. On the console: **Settings → System → Remote Play → Link Device**. It shows an **8-digit code** that expires after a few minutes.
4. Type the code in the five-box field (pasting works) and **Pair**.
5. **PS5 only:** the app asks you to sign in to PlayStation Network first (the pairing needs your account id). **PS4** needs no sign-in.

A wrong or expired code is reported and you can retry. **Forget** (trash icon) removes the pairing; you need a new code to pair again.

### Registering over the internet through PSN (experimental)

For consoles that refuse the 8-digit code (seen on PS5 system software 14.20) pairing goes through your PSN account instead. OpenPS supports this: **Settings → PlayStation Network**.

1. Sign in to PlayStation in your browser, open `ca.account.sony.com/api/v1/ssocookie`, copy all of the text it shows, paste it into **Login token** and press **Connect**. The token is used once to get access and is not stored; it is a login for your whole PlayStation account, so never share it or put it in a screenshot.
2. **Show my PS5 consoles**, then **Register on this Mac** next to your console. You also need to be signed in to PlayStation in OpenPS (the **Sign in** button at the top) and the app must have been built with `--psn-live` and have the key tables (see the README).
3. Wait while it works through the steps (Connecting, Asking your console to join, Waiting for your console, Reaching your console, Registering). It can take a minute or two. **Cancel** stops it at any time. The console must be on, or in rest mode with *Stay connected to the internet*, and signed in to the same account.
4. The console appears under **Consoles**. If you see "could not reach your console", the Mac and the console are probably on different networks or a firewall blocks UDP.

If it does not work, the log (`openps-data/logs/openps.log`) says which step failed. This feature has been tested against fakes built from the original's code and against byte-level references from the original, but **not yet against Sony's servers**: the first real run may reveal differences.

## 3. Play

Click **Connect** on a console card. A console in rest mode shows **Wake & connect**. The connection takes a few seconds; after 8 seconds the app tells you what to check. **Cancel** works at any moment.

While streaming, move the mouse to show the bars (not while keyboard and mouse control is on: then the mouse is game input and they only appear briefly when you switch control on or off). The **top bar** only shows information: console name, frame rate, bitrate, display rate, pacing. The pin at its end keeps it on screen while the controls fade away (remembered). The **bottom bar** is four tools on each side of the **PS button** (centre; press and hold works like the real button), mirrored about it. Left: **Games**, **Edit keys** and **Controls** (keyboard and mouse control on/off, F10). Right: **Stats** (hide or show the numbers in the top bar), **Sound** (mute), **Full screen** and **Leave** (disconnect, or put the console to rest). A tool that is on is lit. Below 760 px wide the captions hide and the tiles shrink so nothing overflows. They hide after 3 seconds.

**Disconnect** asks what should happen to the console: *Disconnect* (leave it on) or *Put the console in rest mode*. After a session the console card waits 10 seconds (the console is still closing the session), then looks for the console again and shows whether it is ready or in rest mode. The games row below uses the same lock.

### Games and trophies

When you are signed in to PlayStation Network, the Consoles page shows a trophy shelf (level, platinum/gold/silver/bronze, games, completed, average progress) and, under your consoles, **one row with the games installed on the console** (the console reports them to your account), each with its poster, trophy progress and size. Use the arrows or scroll sideways. The list and the posters are saved in the data folder (`cache/`), so they look the same on every start; **Update** refreshes them.

* **Play** (the round button on a poster): does what **Connect** does (wakes the console if it is in rest mode, then connects), then starts that game on the console. Starting a game closes whichever one was running. The same lock as the Connect button applies: nothing starts while a session is open, while the console is waking, or during the short wait after a session.
* **Key map** (the drop-down under each game): picks which of your key-map profiles is used for that game. It is applied automatically when the game is started from the app. *Default key map* leaves the profile as it is.
* **Games** (in the stream's bottom bar): the same list on top of the stream, to switch to another game without leaving. The console closes the current game and starts the one you pick.
* If the console says it is **still closing an earlier session**, the connect screen stays up ("Closing the previous session…") and reconnects by itself, for up to a minute and a half. **Cancel** stops it.
* The list needs the PlayStation Network connection (*Settings → PlayStation Network*). Games the console has installed but your account never played have no poster (a coloured tile is shown).

### Controller, keyboard and mouse

When a supported DualSense or DualShock 4 is connected, it controls the game by default. In the stream bar, **Controls** shows the current mode; click it or press **F10** to switch between the controller and keyboard and mouse. Switching modes releases the previous input source.

Open **Settings → Controls** to see connected controllers, their type, slot and battery status, and adjust the stick deadzone. The deadzone ignores small movements around the stick centre and applies immediately.

Console rumble and lightbar feedback are forwarded to DualSense controllers, with adaptive-trigger effects supported by the DualSense HID path. Bluetooth voice-coil PCM haptics are implemented experimentally but have not yet been verified on hardware; USB PCM haptics are not implemented because they use the controller's separate USB audio path.

Keyboard and mouse mapping is **off** when a stream starts, so the mouse stays yours unless you switch to it. When enabled, the pointer is hidden and held in the middle of the window, the mouse drives the right stick, and no key or click reaches anything but the game. Mapping also stops by itself when the window loses focus.

| Key | Action |
|---|---|
| **F10** | Keyboard and mouse on / off |
| **F7** | Next key profile |
| **⌘ + F1 … F6** | Choose profile 1 to 6 |
| **F8** | Show the key map; click a control to rebind it (hold Esc to skip a capture) |
| **F12** | End the stream, the app stays open |
| **F11** | Panic: release everything and close OpenPS |

Default keys (profile `default`):

| PlayStation | Keys | PlayStation | Keys |
|---|---|---|---|
| Left stick | W A S D | Right stick | mouse, I J K L, arrow keys |
| Cross | Space | Circle | E |
| Square | Q | Triangle | F |
| L1 / R1 | 1 / 2, scroll up / down | L2 / R2 | right click / left click |
| L3 | Left Shift | R3 | C, middle click |
| D-pad | Option + arrows | Create / Options / PS | B / Return / ` |
| Touchpad click | M | Mute | T |
| Touchpad tap | 7 | Swipe left / right / up / down | 3 / 4 / 5 / 6 |

Extras: **⌘ + left click** drives L2 and **⌘ + right click** drives R2 as *adaptive triggers* (the value sweeps while held, locks when you release the click, resets when you release ⌘). **Tab + left/right click** toggles R2/R1 and L2/L1 held. A quick click is a press-and-release; a held click stays held.

Profile `racing`: Up arrow = R2, Down arrow = L2, Left/Right arrows = steer, W A S D = camera.

Profile `assassins_creed` (imported from Con2PC, ⌘F3): Space or = Cross, Option or - Circle, C Square, E Triangle, Q = L1, left click = R1, Shift+left click or Z = R2, middle click = R3, Tab = Create, Esc = Options, right Option = PS, H/V/X/B = D-pad up/down/left/right. Everything else keeps the default key.

**Key map names and games.** Names are lowercase snake_case (`night_drive`): whatever you type is converted when you save it (`Night Drive!` becomes `night_drive`), and a name that collides gets `_2`. Every key map has a permanent id, and a game remembers its key map by that id, so renaming a map never breaks a game's choice. If a map is deleted, the games that used it play with the default map, you get one note, and their tile shows "Key map removed, using default" until you pick another. **Add** in the key editor opens a list of the existing key maps (the one you are editing is highlighted); the new map starts as a copy of the one you pick. The shipped `racing` and `assassins_creed` maps are the ones from the installed copy of the app. Older saved choices (by name) are converted to ids automatically.

**Top bar fps.** With frame generation on it reads `30 → 60 fps` (what the game renders, then what is shown), next to the generation icon and its multiple; with it off it is just the game's fps.

**Always 60 fps.** There is no frame-rate setting: the console is always asked for 60 fps. New pictures are passed to the renderer as they arrive; at most one repeated picture is held as a fallback and displayed if no new picture arrives within 100/3 ms (the display thread checks every 50/3 ms). Where generated frames cannot be matched confidently, the nearer real frame is shown as it is (never a blend), so the picture stays crisp. Sharpness defaults to 60 %; saturation and sharpness apply even with upscaling and frame generation off. Settings saved before this keep working: a saved 30 fps becomes 60, and a sharpness still at the old default of 50 becomes 60.

**Frame rate and repeated pictures.** The console sends the stream's rate whatever the game renders, repeating pictures when the game is slower (a game at 30 on a 60 fps stream). The app compares pictures and counts only the different ones (the console re-encodes a repeated picture, so a copy is not identical: it counts as a repeat when it changed far less than the last few new pictures did; measured on a game at about 30 in a 60 fps stream, the copies were every other frame): that is the rate shown in the top bar, the rate frame generation multiplies (37 distinct pictures at 2× is 74), and it is measured from the frames as they arrive, not taken from the Settings value. A mostly still picture (a menu) keeps the last measured rate, and when a scene goes quiet (gameplay to a menu or an intro) no more than three pictures in a row are taken for repeats on the strength of the gameplay's larger changes, so the screen never freezes while the app re-learns what a repeat looks like there. The game's rate goes up at once (read from the last half second of new pictures) and comes down over a couple of seconds, so a game that was loading and starts to render again is not held at its low rate while real pictures arrive faster than they can be taken. Pictures are compared and counted as the frames arrive. Repeated pictures are not queued immediately; the renderer keeps at most one as a fallback, so a repeat cannot push a real picture out of the mailbox. The game's rate is the share of new pictures times the stream's rate (a burst of frames at the start cannot distort it). *Settings → Frame diagnostic → Record 30 s* writes `logs/frame-diag.csv` (frame id, time, changed bytes, new picture) and shows how many frames arrived, how many were new pictures and how many ids were skipped.

### Your own key map

**Settings → Controls → Show in Finder** reveals `input.toml`. It is layered over the defaults, so you only write what differs:

```toml
[bindings]
CROSS = ["SPACE", "ENTER"]

[mouse]
sensitivity = 0.07

[profiles.shooter.bindings]
L2 = ["LEFT_SHIFT"]
```

Reserved keys (F7, F8, F10, F11, F12, ⌘) cannot be bound. A mistake in the file is reported with its line number and the default keys stay in use. **Reset to defaults** deletes the file. To start with mapping on, add `start_enabled = true`.

## 4. Settings

Stream quality (applies at the next connect):

* **Sharpness** (a percentage, 0–200, default 50): with upscaling on, 0 is a plain smooth enlargement, 100 is the GPU upscaler as it draws it (it adds edge contrast the real picture does not have) and more than 100 is sharper still. With upscaling off, 50 leaves the picture alone, more sharpens and less softens.
* **Saturation** (a percentage, 0–200, default 100): 100 is the picture as the console sends it, more is more vivid, less is paler.
* **Frame generation**: *Off*, *1.5×*, *2×*, *2.5×* or *3×* the stream's own rate. A **60 fps** stream gives 60 / 90 / 120 / 150 / 180; a **30 fps** stream gives 30 / 45 / 60 / 75 / 90. The console never sends more than the stream rate; the rest are generated here. The rate you get is the multiple, never above what your display can show (a multiple above your refresh rate is greyed out). It does not have to divide the refresh rate evenly (see *Pacing and the display* below). The overlay shows the rate being presented. Settings saved before this existed keep their old display rate until you pick a multiple.
* **Locked rate:** the rate follows the multiple you chose, times the rate the game really delivers (a game rendering 45 frames a second at *2×* is shown at 90, not 120), and is never above what your display can show. It does not step down by itself. The frame-interval numbers come from when Metal says each frame reached the screen.
* **Pacing and the display:** if the display has variable refresh switched on (System Settings → Displays; the HUD shows "VRR"), frames are scheduled for their moment and the display follows. If it does not, a rate that does not divide the refresh rate (120 on a 144 Hz panel) is paced by the app's own clock instead of the display's: the spacing is even and a frame may tear. Rates that divide the refresh rate are held for whole refreshes and never tear.
* **Interpolation engine** (Settings → Display, only on Macs that have it): *Built-in* (default) or Apple's *MetalFX*. MetalFX is experimental and applies to Smooth mode at about **2×**, where every generated frame is the midpoint between two real ones (it can only make midpoints; at 1.5×, 2.5× and 3× mixing it with the built-in interpolator made the picture flicker, so those use the built-in one). It is never switched off by itself. In our tests it was a little less accurate than the built-in one.
* **Resolution** 360p, 540p, 720p, 1080p. **Bitrate**: *Auto* asks the console for as much as your connection can carry, **up to 100 Mbit/s** (the most the original accepts; a real PS5 ended the stream seconds after the start when asked for 800). It looks at the link per connection (wired link speed, or the Wi-Fi rate, and only for a console on the same network; a console behind a router gets the normal request). The console decides what it really sends. If it refuses the request, or ends the stream within 20 seconds of the start, OpenPS connects again with a normal value and does not raise it for that console again until it is restarted (see `openps-data/logs/openps.log`). Or pick 2/6/10/15 Mbit/s.
* **Codec** HEVC (better quality per bit, used when this Mac can decode it) or H.264. PS4 always streams H.264.
* **HDR** needs HEVC and an HDR display; PS5 only.

Display (applies immediately, also while streaming):

* **Volume**, and mute with the HUD button.
* **Upscaling** (MetalFX): *Off*, *Up to 1440p*, *Up to 4K*. The picture is rendered at the window's resolution, up to the cap. Costs about 1.3 ms (1440p) or 2.3 ms (4K) of GPU time per frame.
* **Frame generation**: *Off* shows frames as they arrive (up to 60 per second); *1.5×* to *3×* generate extra frames, because the console never sends more than 60.
* **Extra frames**: **Smooth** (the default) interpolates between frames: the cleanest picture, at the cost of **one source frame (about 17 ms at 60 fps) of latency**, which stays low enough to feel responsive. **Low latency** predicts forward from the last frame and adds no delay but can show small artifacts around fast edges. Measured on real footage, interpolation clearly reduces error and extrapolation only marginally beats repeating a frame; see `analysis/spec/06-frame-generation-research.md`.
* **Camera latency compensation** (on by default): turns the picture immediately when you move the mouse, before the console answers. It learns how your game responds and switches itself off when it cannot tell. Not validated against a real game.

## 5. Troubleshooting

| Symptom | What to do |
|---|---|
| Console not listed | Same network and subnet? Local-network access allowed for OpenPS (System Settings → Privacy & Security → Local Network)? Remote Play enabled? Try **Refresh**, or add it by IP address. |
| "Not seen on your network" | It is off, or on another network. Rest mode works if *Enable turning on PS5 from network* is on; use **Wake & connect**. |
| "registration rejected (HTTP 403, code 80108bff)" | The console did not accept the credentials. Use a NEW code (open **Link Device** again), and make sure the PSN account you signed in with is the account that is signed in on the console (and the user chosen for Remote Play if there are several). The app sends exactly what the original does; see `crates/openps-rp/tests/oracle/README.md`. |
| "registration rejected … code 80108b02" | The PSN account is not one that is signed in on the console, or the code was wrong. Sign in to the console with that account, or sign in here with the console's account. |
| Wrong or expired code | Generate a new one (**Link Device**) and try again within a few minutes. |
| Top bar says "PSN unavailable" | Hover it for the reason. Usually: the build lacks `--psn-live`. Reinstall with `scripts/install.sh`, which always includes it. PS4 pairing does not need PSN. |
| "stored registration keys are missing" | Forget the console and pair it again. |
| Stuck on "Connecting" | Wait for the hint at 8 s, check power and network, **Cancel** and retry. Try 720p / 30 fps / 6 Mbit/s on a weak network. |
| Artifacts or stutter | Lower the resolution or bitrate; use Ethernet; set *Frame generation* to Off and *Extra frames* to Smooth to compare. |
| Artifacts only for the first moment | Normal while the first frames arrive in a burst; fixed in this build, see the research notes. |
| Mouse is stuck in the window | Press **F10**; **F11** closes the app; leaving the window also releases it. |
| "Your key map has a mistake" | Open it from Settings → Controls and fix the line shown, or reset it. |
| No sound | HUD shows "No sound" if audio could not start; check the output device and the volume setting. |

Logs: `openps-data/logs/openps.log` (inside the app). They hold no keys, PINs or tokens. Nothing is sent anywhere.

### Controller input diagnostics

If a physical controller seems to send the wrong thing, start the app from a terminal with tracing on (a double-click launch cannot pass the variable):

```sh
OPENPS_INPUT_TRACE=1 ./install/OpenPS.app/Contents/MacOS/openps-app
```

Every pad change is then logged with a microsecond clock (`t=`) at each stage: `source_emit` (what macOS reported), `arbiter` (stored state, and whether it was forwarded), `feedback_encode`, `wire_protect` (one line per history packet: its sequence, key position and records; a change that moves several controls at once is sent as one packet per control, because the console applies only one new event per packet; trigger records are `86` for LT and `87` for RT, last byte the value) and `udp_send` (whether the socket accepted that same packet). While tracing, the log file is not rotated, so it grows; delete it afterwards. Packets the socket refuses for transient reasons are logged as warnings even without tracing. A `udp_send` line proves the packet left the Mac, not that the console applied it.

## 6. Uninstall

`scripts/uninstall.sh [--dest DIR] [--keep-data]`, or just delete `OpenPS.app`. All data is inside it.
