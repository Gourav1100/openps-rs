# OpenPS cross-platform media, input, scene intelligence, and performance specification

**Status:** proposed implementation specification, not an implementation or benchmark report. **Prepared:** 17 November 2026. **Repository:** [Gourav1100/openps-rs](https://github.com/Gourav1100/openps-rs). **Audited snapshot:** `a27431fbe84b3848d06ad3ea284d85e01773f2ef` (main at inspection). Code links below are pinned to that snapshot. New paths explicitly marked “proposed” do not exist yet.

## 1. Outcome and boundaries

Deliver usable PlayStation Remote Play clients on Windows, Linux, Android phones/tablets, Android TV/Google TV, and iOS/iPadOS while preserving macOS. Each supported platform must pair, connect, decode and display H.264/HEVC when its decoder supports the negotiated profile, play audio, send mapped keyboard/mouse and controller input, disconnect cleanly, and recover from lifecycle and network changes. “Builds successfully” and “opens a Tauri window” are insufficient definitions of support.

Provide Vulkan, DirectX, and OpenGL-family rendering where appropriate, spatial upscaling, and video-derived motion-compensated frame generation. Keep Metal as the Apple production backend. DirectX means D3D12 presentation/processing initially, with D3D11 decoder interop; it does not promise every DirectX version. On Android, OpenGL means OpenGL ES. Do not advertise native DirectX on Linux/mobile, or native Vulkan on iOS. A Vulkan-over-Metal experiment can be tracked separately without blocking the native iOS client.

Keyboard/mouse capture means relative mouse movement and native physical keyboard events routed to the existing pad mapper while the gameplay view has focus, with the cursor hidden/locked where the OS permits. It does not mean trapping system shortcuts, suppressing Home/notification gestures, injecting into other applications, or preventing the user from leaving. Preserve a reliable release control on every device.

Interpret fixed landscape as a landscape gameplay canvas with no automatic portrait gameplay rotation; request landscape orientation for the mobile shell and keep both landscape directions available for ergonomics. If a strict single landscape side is desired, expose a persisted left/right preference. The OS may still resize or orient the app window; in that case preserve the landscape content rectangle with letterboxing and accessible controls. A universal promise to force every iPad/foldable/window into landscape is not technically supportable.

The stream contains decoded color pictures, not game-engine geometry, jitter, depth or renderer motion vectors. Therefore the initial upscaler is spatial FSR EASU/RCAS or equivalent, and frame generation estimates motion from video. Do not label this integration “FSR 3 frame generation,” DLSS, or XeSS. AMD documents depth and motion-vector inputs for the standard temporal/FG paths; inventing these inputs would make a research project, not a correct integration. [FSR spatial](https://gpuopen.com/manuals/fidelityfx_sdk/techniques/super-resolution-spatial/), [FSR 3 integration](https://gpuopen.com/fidelityfx-super-resolution-3/).

## 2. Audit: existing architecture and concrete gaps

The existing strengths are valuable: protocol/application separation, a sans-I/O input engine, bounded media delivery, IDR recovery, a content clock, a duplicate-frame gate, adaptive rendering, camera compensation, a four-state scene filter, and extensive deterministic tests. Extend these components instead of replacing the protocol stack.

| Evidence in current tree | Consequence and required change |
|---|---|
| `src-tauri/Cargo.toml` makes video/audio/macinput/metal/scene dependencies macOS-only | Declare portable contracts unconditionally and native implementations per target. Merely broadening `cfg` guards would pull AppKit/IOKit into mobile builds. |
| `src-tauri/src/lib.rs::media_and_events` returns `NullMedia` off macOS | Implement a platform media factory and explicit unavailable/error state; a stats-only sink is not platform support. |
| `media_and_events`, `build_core`, and setup reference `openps_metal::look::SharedLook` outside macOS guards | Extract `SharedLook` into a portable render/input contract and remove these compile-time dependencies. |
| `src-tauri/src/paths.rs::ensure` and `crates/openps-core/src/store.rs::write_private` import Unix filesystem extensions unconditionally | Windows compile blockers and storage semantics need native implementations and atomic replacement tests. |
| `openps-video::{pipeline,media}` use concrete `vt::Decoder`/`vt::Frame`; `vt::Frame::id` recovers identity from `pts / 1500` | Separate identity from timestamps; make hardware frame ownership explicit; make decoding pluggable. |
| `src-tauri/src/video.rs::Video::install` constructs `NSView`, hosts `CAMetalLayer`, and calls `set_bgra` | Keep this as the macOS adapter. Implement HWND/Wayland/X11/Android Surface/UIView hosts separately. Make YUV the preferred decoded format. |
| `openps-audio` gates Opus decode and playback to macOS | Audio is a first-class port, not a rendering follow-up. Add portable Opus and native output backends. |
| `openps-metal/src/renderer.rs` includes policy, presentation, imports, duplicate detection, and statistics | Extract policy while leaving Metal resource ownership intact until parity tests pass. |
| `openps-scene/src/service.rs` retains raw CVPixelBuffer pointers and calls Darwin QoS APIs | Replace the service-facing picture type and scheduler with portable interfaces; retain Core ML as one encoder. |
| `openps-scene/src/encoder.rs` concatenates 512 MobileCLIP + 1536 DINOv2 features, with whole-frame ScaleFill preprocessing | Preserve exact layout and preprocessing in every model export; do not assume an ONNX conversion is numerically equivalent. |
| `tools/scene-context/scenelab/evaluate.py` holds out recordings and simulates actions for sequence replay | Existing evaluation is useful, but real action traces and game-disjoint splits are needed to measure generalization and control-switch reliability. |
| `dist/app.js` contains MetalFX/FSR-specific availability and explanatory text | Add backend capabilities and requested-versus-effective settings, then adapt phone/TV layouts. |
| No tracked `.github` workflow directory or `assets/models` directory in this snapshot | Add CI and a reproducible model packaging/download mechanism; do not assume the shipped ML weights are available. |
| README says real-console validation has not occurred and references `../analysis/spec` outside the repo | Treat PS4/PS5 hardware validation and self-contained documentation as release gates. README also contradicts itself about bundled constants and licensing. |

These are static findings. No real console, Windows/Linux/mobile hardware, measured speedup, or successful cross-platform build is claimed by this specification.

## 3. Platform/backend contract

Proposed minimum engineering baselines (not existing support): Windows 10 22H2/11 x64, Linux x86_64 with a defined supported distribution matrix, Android API 26+ arm64, Android TV API 26+ arm64 with armv7 investigated only if actual target hardware requires it, and iOS/iPadOS 16+ arm64. Final minimums must be validated against the chosen Tauri, GPU, model-runtime and codec dependencies. Test current OS releases too. Windows ARM64 and Linux ARM64 are follow-up builds after x64, tracked within platform tasks; do not mark them supported by inference from Rust target availability.

| Platform | Production graphics | Decoder | Audio | Input and limitations |
|---|---|---|---|---|
| Windows | D3D12 default; Vulkan selectable; OpenGL compatibility path | Media Foundation hardware MFT with D3D11 surfaces; optional packaged software fallback | WASAPI via a validated output adapter | Raw Input + focus/capture management; native physical keys; XInput/GameInput or HID controller adapter |
| Linux | Vulkan default; OpenGL/EGL compatibility | FFmpeg/libavcodec with VA-API where available; device-qualified other hardware paths; software fallback | PipeWire/PulseAudio/ALSA via selected library/backend | Wayland relative-pointer + pointer-constraints; X11 raw motion/grab; no global Wayland warp assumption |
| Android | Vulkan when import/driver tests pass; GLES compatibility | MediaCodec surface output; negotiate low-latency mode if supported | Oboe/AAudio | Native focused-view pointer capture and physical keyboard; app lifecycle and device disconnect reset input |
| Android TV | Same native stack, conservative GPU tier | MediaCodec, model-specific profile/resolution checks | Oboe/AAudio with HDMI route handling | D-pad/remote mandatory; controller/keyboard/mouse optional; no touchscreen dependency |
| iOS/iPadOS | Metal production | VideoToolbox with CoreVideo textures | AudioToolbox/AudioUnit with AVAudioSession | GCKeyboard/GCMouse and UIKit pointer-lock preference when available; verify separately on iPhone and iPad |
| macOS regression target | Existing native Metal/MetalFX | Existing VideoToolbox | Existing AudioToolbox, refactored behind contract | Existing macinput adapter |

Support levels must be reported separately: native decode, native texture import, presentation, spatial upscale, compute motion generation, scene inference, relative mouse capture, and controller extras. Graphics API availability alone proves none of the other capabilities.

A modern Rust GPU abstraction such as wgpu is the recommended shared implementation for Vulkan/D3D12 and optional Metal experiments. Its documented backends include Vulkan, D3D12, Metal and GL, but support and limits differ; its current GL route on Windows is through ANGLE, so “GL selectable” must not silently mean the requested native desktop OpenGL driver. Use a small explicit GL/GLES backend when native GL/EGL or external-texture import requires it. Pin the chosen crate version after interop spikes, rather than adopting “latest” blindly. [wgpu backend documentation](https://docs.rs/wgpu/latest/wgpu/struct.Backends.html).

## 4. Target decomposition and migration rules

Proposed layout:

```
crates/openps-media/                  # shared frame/format/clock/capability contracts
crates/openps-render/                 # portable timeline, quality policy, graph, look
crates/openps-render-wgpu/            # Vulkan and D3D12 shared shader backend
crates/openps-render-gl/              # desktop GL/GLES fallback and external textures
crates/openps-input-host/             # event normalization, capture state, runner
crates/openps-video/src/decoder/      # apple, windows, linux, android, software
crates/openps-audio/src/output/       # apple, wasapi, linux, android
crates/openps-scene/src/encoders/     # coreml, onnx, optional mobile runtime
src-tauri/src/platform/               # factory + windows/linux/macos/android/ios hosts
src-tauri/plugins/openps-platform/    # proposed local Tauri plugin: Kotlin/Swift bridge
src-tauri/gen/android/                # generated project; preserve customizations reproducibly
src-tauri/gen/apple/                  # generated project; verify actual generator layout
```

Use new crates only for dependency boundaries with a real consumer. Decoder adapters can remain modules in `openps-video`; do not create one crate per API merely for symmetry. Retain `openps-input::engine::Platform`, `PadSource`, `PadSink`, `MediaSink`, and existing protocol interfaces where their semantics suffice. Move pure `timeline`, `ladder`, `camera`, `look` logic out of `openps-metal` through compatibility re-exports, with a test-first mechanical extraction. Move `RenderSettings` and `RenderStats` after separating native enums/handles. macOS must continue working at every merge.

The platform factory creates a capability snapshot and a session-scoped media graph. The graph owns decoder, renderer, audio, scene tap and cancellation. The shell owns native views and main-thread lifecycle. The Rust core never owns a UIKit/Android activity pointer directly. Native adapters expose opaque, reference-counted handles with documented destruction threads; callbacks carry a generation token and cannot publish into an obsolete session. Never use blanket `unsafe Send/Sync` to bypass actual thread-affinity requirements.

Retain Tauri for settings and the library. Use a native gameplay surface under or alongside the webview with explicit composition and hit-testing. Verify transparent overlays on each OS early; if a WebView cannot reliably coexist with the surface, use a dedicated native gameplay controller/activity and retain the Tauri shell for navigation. Do not move pixels or 1 kHz input through JSON IPC. Kotlin/Swift plugins are supported extension points, but native surface integration still needs a proof-of-concept. [Tauri mobile plugins](https://v2.tauri.app/develop/plugins/develop-mobile/).

## 5. Media types, time, synchronization and failure behavior

Define `SessionEpoch(u64)`, `FrameId { sequence: u64 }`, `MediaTime { ticks: i64, timebase_num, timebase_den }`, and a monotonic local clock. Unwrap 16-bit wire IDs once per stream epoch. The current `id * 1500` value is a frame-identity encoding, not a universally valid capture timestamp: stop using it as both. Preserve compatibility conversion only inside the old VT adapter during migration. If the wire does not supply trusted capture PTS, reconstruct cadence from negotiated stream FPS and measured arrivals, and expose that timestamp as estimated. Distinguish packet arrival, access-unit completion, decode enqueue/completion, render enqueue, GPU completion, scheduled present and observed present.

`EncodedAccessUnit` contains epoch, sequence, codec, bytes, parameter-set revision, reference/keyframe status, estimated/source time and discontinuity flags. Reuse current NAL parsing and loss/IDR policy. `DecodedFrame` contains an ownership lease, frame identity/time, coded size, visible crop, rotation, pixel format (NV12/P010/BGRA), color primaries, transfer function, matrix, range, and synchronization metadata. Native handles are backend-specific enums behind target gates, not public raw pointers. CPU planes include row stride and validated plane bounds. No unknown format may be interpreted as BGRA.

Decoder contract: configure, enqueue, poll/callback output, flush/discontinuity, and shutdown. Distinguish `NeedKeyframe`, `UnsupportedFormat`, `DeviceLost`, `TemporaryBackpressure`, corrupt bitstream and fatal initialization. Enqueue must not wait for the entire asynchronous hardware queue. Use bounded submission and callback queues with generation checks. Keep the existing 500 ms keyframe throttle initially; resetting one decoder must not create a request storm. Codec/profile or size changes create a new format generation and invalidate render/scene history.

Texture import contract: check format and device compatibility; retain the decoder lease until GPU use completes; wait on acquire synchronization in the GPU queue; return release synchronization before recycling. On failure, choose an explicit one-copy path and report it. “Zero-copy” is an observable path, not a universal promise. Measure whether a copy releases scarce decoder surfaces earlier and reduces latency; the existing Metal ring has a legitimate ownership purpose.

Queues have maximum age as well as count/bytes. Proposed starting bounds: decoded mailbox 2–3 pictures, renderer 2–3 frames in flight, one pending scene sample, no unbounded compressed-frame accumulation. Tune AU queue bounds to codec reference dependencies: arbitrarily dropping a P-frame before decoding can corrupt the reference chain; drain/decode without presenting when affordable, otherwise enter IDR recovery. Prioritize stop/input release/lifecycle over stale video. Keep audio independently bounded. Reserve control capacity and enforce queue-accounting invariants under failed sends and shutdown.

On suspend: release capture and send neutral pad state first, stop new decode, cancel inference publication, stop audio, release native surfaces and bounded GPU ownership, then stop/reconnect transport according to policy. On resume: recreate surfaces, increment epoch, clear temporal histories, obtain fresh stream info/IDR and reacquire capture only through a valid focused user action. A lost GPU device must cause bounded teardown/reselection, never a tight restart loop.

## 6. Decoder and graphics implementation by platform

### Windows

Implement `decoder/windows.rs` around Media Foundation MFT capability enumeration and asynchronous output handling. Feed length-delimited or Annex-B data according to negotiated MFT requirements, retaining the existing parameter-set analysis. Hardware H.264 and HEVC availability is device/OS-dependent; absence of a suitable HEVC decoder must negotiate H.264 or give an actionable error. Share a D3D11 device through the DXGI device manager where applicable. [Microsoft decoder/device-sharing overview](https://learn.microsoft.com/en-us/windows/win32/medfound/supporting-direct3d-11-video-decoding-in-media-foundation).

Build D3D11-decoder-to-D3D12 interoperability as an explicit spike: shared resources, adapter identity, supported texture format, fence/keyed-mutex semantics, and decoder-surface recycling. Not every decoder texture is shareable; a same-GPU copy into an application-owned shared resource is a valid intermediate path. Vulkan import of Windows resources is a separate capability, not implied by D3D12 support. Always retain a CPU/upload fallback for diagnosis where licensing and codec implementation permit it.

Own the swapchain and native child window correctly relative to WebView2; handle DPI changes, monitor migration, HDR/SDR selection, minimize, fullscreen transitions and device removal. Begin with SDR. Choose frame-latency and vsync settings based on measured presentation latency and tearing policy, not generic “uncapped” defaults. Raw Input must not double-count DOM events; focus loss releases mouse confinement and all pressed keys. Packaging must handle a read-only install location and required WebView runtime.

### Linux

Implement FFmpeg decode behind a narrow wrapper and configure only required codecs/features. Start with software decode and Vulkan upload to prove the entire flow, then VA-API surface export/import with DRM format modifiers, plane offsets/strides, device matching and synchronization. DMA-BUF import requires actual driver support; never assume all NV12 exports have a linear modifier. NVIDIA decode needs its own qualified path or documented fallback. Avoid a mandatory dependency on Vulkan Video for initial coverage.

Support Wayland and X11 separately. Wayland compositors control capture and window placement; use relative-pointer and pointer-constraints protocols and show capture unavailable if absent. Test GNOME/KDE and X11. Linux audio must survive server restart and default-device changes. Package system library dependencies explicitly; select a first distribution baseline, then test an AppImage/Flatpak or distro package according to dependency and hardware access requirements. A sandbox must have the needed GPU/audio access without global keyboard permissions.

### Android phones and tablets

Create a Kotlin/native plugin hosting the gameplay surface and forwarding lifecycle/input through a small FFI interface. MediaCodec runs off the UI thread. Validate codec MIME/profile/level, adaptive-size changes, output crop and actual output timestamps. Low-latency decoder support is a runtime capability; API 30 introduced the relevant feature, so API 26 baseline devices need a fallback. [MediaCodec capabilities](https://developer.android.com/reference/android/media/MediaCodecInfo.CodecCapabilities.html), [MediaCodec lifecycle and surfaces](https://developer.android.com/reference/android/media/MediaCodec).

Two deliberate paths: (A) direct MediaCodec-to-display Surface as a basic low-overhead fallback, with postprocessing unavailable; (B) processable output to SurfaceTexture/EGL for GLES or a validated ImageReader/AHardwareBuffer path for Vulkan. Output-surface selection can require decoder reconfiguration, so do it at connection or through a bounded restart when enabling processing. AHardwareBuffer availability does not prove the selected codec can export a sampleable buffer. Test MediaCodec producer compatibility, external format/YCbCr conversion, synchronization and maximum outstanding images. If Vulkan import fails, select GLES or a measured copy route, not a black screen. Consult [Khronos Vulkan external-memory specification](https://registry.khronos.org/vulkan/specs/latest/pdf/vkspec.pdf).

Handle surfaceDestroyed without using a stale ANativeWindow, activity recreation, background/foreground, audio focus, screen-off, low-memory callbacks, thermal status, Wi-Fi changes and network reconnect. Build native libraries for supported ABIs with 16 KB page-size-compatible packaging/toolchain validation. Do not use a persistent background service just to keep invisible gameplay rendering alive. Keep screen awake only during active foreground play.

### Android TV and Google TV

Reuse Android media/input code, but ship a separately tested TV product mode. Add Leanback launcher, TV banner and touchscreen-not-required declarations. Every flow—pairing, sign-in, code entry, console selection, connect, settings, release capture and disconnect—must be possible from D-pad and Select/Back. Never require hover, a mouse, a function key, a pinch gesture or a touch keyboard. The launcher requirements are documented in [Android TV setup](https://developer.android.com/training/tv/get-started/create).

Use visible focus rings, deterministic focus order, remote repeat throttling, large targets, overscan-safe margins and a 10-foot settings layout. Set conservative defaults: native resolution, no FG, economical scene inference. Enable processing only after capability and sustained-load qualification. Distinguish 4K panel size from 1080p stream/decode capacity. Test HDMI hotplug, 59.94/60 Hz output, audio route changes, sleep/resume, Bluetooth reconnect and low-memory dongles. Sign-in may offer a secure companion-code flow if supported by the existing authentication protocol, but must not invent a Sony authorization endpoint.

### iOS and iPadOS

Reuse portable NAL/pipeline logic and adapt the VideoToolbox implementation for iOS explicitly. Reuse CoreVideo texture-cache/IOSurface ownership patterns only where the API is available. Replace AppKit `NSView`/`NSScreen`/CoreGraphics capture code with UIKit `UIView`/`UIViewController`, `CAMetalLayer`, per-window scene geometry and display timing. Update build scripts and Cargo target tables for Apple mobile frameworks without enabling macOS IOKit. Audit MetalFX class/method availability at runtime and across SDK deployment targets; keep the built-in GPU algorithm as the fallback.

Configure AVAudioSession playback and interruption/route-change handling, write caches and settings only under the application sandbox, and use Keychain for credentials. Native onboarding must handle local-network permission denial and recovery. LAN discovery/wake methods need entitlement/API validation; where broadcast/multicast is unavailable, support explicit console address entry and report the limitation. Keep sign-in redirect handling in native lifecycle callbacks rather than assuming a desktop popup works.

iPhone and iPad are separate input test targets. A GCMouse declaration is not proof of useful pointer capture on every device/OS combination. Probe available physical devices and native API availability; validate sustained relative movement on actual hardware. If the OS does not provide capture, explain that limitation and retain controller/touch or ordinary pointer navigation rather than falsely showing “locked.”

## 7. Locked keyboard/mouse and landscape state machine

Extend existing `Engine<Platform>` rather than reimplementing mapping. Extract reusable hub/runner behavior from `openps-macinput`, keeping Darwin-only cursor and event APIs there. Normalize native physical keys to existing `Key`/`Input` enums; keep text input, IME and Unicode editing separate. Do not use character values for WASD bindings. Normalize wheel, buttons, timestamps, repeat semantics and relative delta units with a documented sign convention. Calibrate DPI/sensitivity without double-applying system acceleration.

Capture states: `Unavailable`, `Released`, `Requested`, `Captured`, `Suspended`. Gameplay mapping may activate only after native capture/focus confirmation if capture was requested. Enter on explicit Play/capture action; leave on release button/chord, focus loss, app pause, device disconnect, system modal, session termination or native capture loss. On every leave, clear held keys/buttons, discard residual mouse deltas, and send a neutral controller state. On reacquire, reset the mouse accumulator so the first sample cannot jump the camera. Keep an on-screen release affordance usable without a keyboard and a remote Back escape on TV. Preserve existing panic behavior without porting “kill target application” semantics to mobile.

Android: use the focused native view's `requestPointerCapture`, `onPointerCaptureChange`, and captured-pointer callback. Only report captured after confirmation; release on focus loss and respect system release. Native physical key events drive the mapper while UI text fields keep normal editing. Verify Bluetooth and USB mice/keyboards, Alt/Shift/Ctrl, international layouts, scroll, extra buttons, reconnect and high polling rates. Android documents capture from API 26 and requires focused view hierarchy. [Pointer capture](https://developer.android.com/develop/ui/views/touch-and-input/gestures/movement).

iOS/iPadOS: use GCKeyboard key transitions and GCMouse relative movement in native code, with a view controller that expresses `prefersPointerLocked` and requests an update when gameplay capture changes. Do not infer actual capture from the preference flag alone. Observe focus/scene activation and mouse connection state; test modal presentation because UIKit can release pointer lock. Use a native gameplay controller if the Tauri root controller cannot expose the necessary preference cleanly. [Apple mouse input guidance](https://developer.apple.com/videos/play/wwdc2020/10094/), [pointer-lock update API](https://developer.apple.com/documentation/uikit/uiviewcontroller/setneedsupdateofpreferspointerlocked%28%29).

Android landscape: request `sensorLandscape` (or an explicit saved landscape side); set a legitimate game category only if appropriate for the product. Android 16 large-screen restrictions and Android 17 opt-out removal mean the manifest alone is not a cross-device guarantee. Maintain landscape letterboxed gameplay inside any imposed window bounds; relayout overlays, touch coordinates, safe areas and capture transforms on resize. Test fold/unfold, tablet multitasking and desktop mode. [Android 16 behavior](https://developer.android.com/about/versions/16/behavior-changes-16), [Android 17 orientation changes](https://developer.android.com/about/versions/17/changes/ff-restrictions-ignored).

iOS landscape: declare landscape supported orientations for iPhone and iPad and implement controller orientation preferences. Use availability-gated scene orientation-lock APIs where supported, and support resized windows. Do not base the design solely on `UIRequiresFullScreen`, which Apple now deprecates. Resolve launch screen, sign-in, software keyboard and alert presentation without a portrait-to-landscape flash or incorrect input transform. [Supported orientations](https://developer.apple.com/documentation/BundleResources/Information-Property-List/UISupportedInterfaceOrientations), [full-screen key migration](https://developer.apple.com/documentation/bundleresources/information-property-list/uirequiresfullscreen).

## 8. Rendering graph, upscaling and frame generation

Shared graph: native texture import → color conversion/crop → source-frame analysis → optional motion generation → spatial upscale → bounded sharpen/color finish → local UI composition → present. Each pass declares input/output format, size, synchronization and lifetime. Use linear light only in stages designed for it; apply the correct transfer conversion around them and avoid accidental double-gamma. Start with verified SDR BT.709 and limited/full range handling; make HDR/P010 an explicit capability with tone mapping and tests before enabling it.

Upscaling: port existing EASU and RCAS behavior, including coefficient calculation, edge handling, user sharpness and the distinction between enlargement and downscaling. Keep bilinear/native fallback everywhere. Validate shader outputs on impulse, checkerboard, diagonals, text, saturated colors and real clips. Compute backends use WGSL equivalents; GL/GLES fallback can use fragment passes for spatial filters. Do not require compute support merely to display video or upscale. Map nominal scale factor to actual drawable/crop dimensions and clamp allocations by device limits and memory budget.

Frame generation has two independent modes. Interpolation waits for two real pictures and presents intermediate times; it adds roughly one source interval before other buffering (about 33.3 ms at 30 fps or 16.7 ms at 60 fps). Extrapolation predicts forward from available motion; it reduces that lookahead but introduces prediction artifacts. Rename user-facing “ZeroLatency” wording to describe this tradeoff accurately—network, decode, playout and presentation still have latency. Existing mode names remain readable during settings migration.

Port current luma/motion-search/warp and camera-compensation logic into a shared algorithm specification and parity fixtures before rewriting shaders. Maintain flow/history at source or reduced resolution; reuse per-real-frame analysis across multiple generated frames. Require forward/backward flow consistency, confidence masks, disocclusion handling, border clamping and a fallback to the latest real frame. Do not feed generated frames into the decoder, duplicate detector or semantic classifier. Never synthesize across epoch/format changes, IDR discontinuities, long stalls or detected hard cuts.

Add a fast per-real-frame shot-cut detector based on downsampled luminance/chroma changes and robust motion residuals. This is distinct from the semantic 2 Hz gameplay/menu classifier. Flashes, fades, loading screens and rapid camera turns need separate tests to avoid excessive history resets. Local UI is composited after FG so its cursor/text remain crisp; HUD baked into the console picture cannot be separated perfectly, so conservatively reduce warping in static high-contrast regions and disclose residual artifacts.

Advertise FG only when the backend's compute/storage/texture limits and measured frame budget allow it. GL/GLES 3.1-class compute or desktop compute support may enable a separate qualified motion path; low-tier GLES devices still get native video and spatial upscale. Plan and test this path, rather than declaring FG supported on every GL device. Limit initial production generation to a tested 2× mode; retain current higher/fractional multipliers only where pacing and artifact tests pass. Never count generated frames as new game simulation or improved input response.

Presentation follows the actual output display, including 59.94, 60, 90, 120, 144 Hz and variable refresh. Carry over content cadence and duplicate detection so 30 fps content transported at 60 fps is not mistaken for 60 unique frames. Prefer native display pacing signals where available; do not port a macOS spin/sleep policy unchanged to phones. GPU timestamp support is optional and must have a CPU-side fallback. Cache pipelines and bind groups/descriptors; do no shader compilation or large allocation in the steady-state frame loop.

## 9. Scene intelligence: changes justified by current implementation

### 9.1 Portable and reproducible inference

Keep `Probe` and `SceneFilter` platform-neutral. Add an `ImageEncoder` interface receiving a sampled real decoded frame/preprocessed tensor and returning a versioned embedding. Core ML remains the Apple implementation. Export equivalent ONNX graphs for desktop/Android CPU baseline and evaluate acceleration providers per device; provider names are not performance guarantees. A LiteRT or other mobile implementation should be selected only after export/parity and sustained-load tests. [ONNX Runtime execution providers](https://onnxruntime.ai/docs/execution-providers/).

Create a model manifest with SHA-256 hashes, format/version, source revision, licenses, tensor names/shapes/dtypes, RGB ordering, range/normalization, resize filter, ScaleFill geometry and 512+1536 concatenation order. Bundle or reproducibly retrieve weights; the audited tree lacks `assets/models`. Use content hashes instead of only newest-file-time plus size (`encoder.rs::package_stamp`), which can collide for changed packages. Compile/cache atomically, validate loaded dimensions and limit cache growth. Optional model download requires explicit UI action; never block first gameplay on a network model fetch.

### 9.2 Staleness, ordering and failure recovery

`SceneService` currently has one replaceable pending picture and a 0.5-second offer interval, which is a good bounded design. It checks `link.running()` before publishing, but there is no frame/profile epoch in `Picture`. If detection turns off and back on, the profile changes, or the stream reconnects during inference, “running” alone cannot distinguish old work. Add session/profile/model generations to samples and results; recheck them before publish. Reset filter/action history and pending picture on generation changes. Drop results older than a configured maximum age, initially 1 second, measured from original sample time.

Inference errors currently log and continue. Repeated failures can leave a previously published scene looking current. Introduce consecutive-failure/staleness state, throttled logs, retry/backoff, explicit Unavailable/Degraded feedback, and a safe base/manual layer when freshness expires. A model load failure must be recoverable without restarting the application. Publish state/confidence/age/generation as one coherent snapshot (packed atomics or immutable snapshot), not an accidental mix from concurrent updates. Ensure the input loop never waits on model inference.

### 9.3 Confidence and control correctness

`Probe::from_json` checks dimensions and some ranges, but should reject empty/duplicate/missing context classes, nonfinite values, invalid positive temperature and ill-conditioned scale values. Keep public inference fallible on malformed embedding length/nonfinite values rather than relying on assertions for external model output. Normalize and validate distributions; test extreme logits and missing classes. Precompute scaled coefficients/biases once to avoid repeated standardization and temporary vectors, but measure this separately from much larger encoder cost.

The filter deliberately favors button cues and defaults to a 12-second exit lock and tiny spontaneous foot-to-vehicle rate. These are game-specific assumptions. Preserve them as the compatibility profile; fit per-game parameters only with held-out evidence, constrain values, and always offer manual layer override. Evaluate failed entry, repeated presses, involuntary exit, scripted vehicle entry, vehicles adjacent to the player, menu transitions and long cutscenes. Confidence needs calibration against held-out labels; add entropy/margin-based abstention and separate “context estimate” from “mapping layer selected.” When uncertain or stale, hold a safe layer briefly then use configured fallback; never oscillate mappings on every argmax change.

The current `SceneContext::layer` maps cutscene to menu. Make that an explicit profile policy. Changing a layer while a key is down must send releases for old bindings and coherent state for new bindings without accidental extra button presses. Profile configuration, allowed contexts, action availability and model generation must update atomically from the detector's perspective.

### 9.4 Evaluation and mobile cost

Keep existing Rust/Python reference and replay tests. Add real physical/mapped action timestamps, packet-loss/reconnect annotations and human-labeled transition intervals. Split by game, recording/session and source clip; prohibit adjacent frames and alternate encodes of the same clip crossing train/test boundaries. Keep a final untouched test set. Report macro-F1, per-class precision/recall, calibration error, false mapping switches per minute, transition-delay p50/p95, stale-result count, inference p95, RAM, accelerator utilization and sustained power/thermal behavior. Existing historical results are not a benchmark of this proposed system.

Ablate current two-model design against MobileCLIP-only, a distilled student, reduced input sizes and FP16/INT8 exports. Existing `pictures.py` records that corner crops previously failed to help: do not prescribe adding HUD crops without a new controlled experiment. Retrain the probe when encoder/layout/quantization changes; never zero-fill the DINO portion and call it equivalent. Initial sampling remains 2 Hz; permit temporary bounded bursts after meaningful actions and reduce sampling under thermal pressure. A cheaper first-stage classifier may invoke the large encoder only for uncertainty. Select the smallest model meeting switch-error targets, not the largest model fitting memory.

## 10. Performance improvements with evidence and validation

| Priority | Current evidence | Proposed improvement | Proof required |
|---|---|---|---|
| P0 | `vt.rs::decode_avcc` waits for asynchronous frames on each decode | Submit asynchronously with bounded completion handling | Decode p95 and end-to-end queue age improve; callback lifetime/rebuild tests stay correct |
| P0 | `media.rs` serializes pipeline access and non-Apple media is absent | Dedicated decoder ownership plus platform contracts | No UI/network thread blocking; loss/flush/stop races tested |
| P1 | Metal output requests BGRA; ring copies decoded pictures | Prefer NV12/P010 import + GPU conversion; compare retained zero-copy against one-copy ownership | GPU bandwidth, decode surface retention and total latency measured; no use-after-recycle |
| P1 | `DupGate::sample` locks IOSurface and allocates sampled bytes per real frame | Reuse fixed storage; consider GPU downsample/signature with asynchronous tiny readback | No whole-frame readback; identical repeat decisions or qualified improved thresholds |
| P1 | Native wait/spin loop and fixed 5 Hz adaptive ladder (README still says 15) | Display-driven pacing; power-aware policy; cap against actual content/refresh | Present-interval distribution, missed deadlines and thermal soak improve |
| P1 | Audio uses 40 ms prefill, 60 ms target, 150 ms max and try-lock callback | Tune by route, use preallocated SPSC PCM ring, add drift correction and lock-miss metrics | Under/overruns, A/V offset and audio latency measured under load; callback has no allocation/blocking |
| P1 | Session has an 8192-item inbound threshold and mixed control/media processing | Queue-age limits, control priority, bounded command coalescing, byte budget | Stop/neutral input is prompt under packet flood; transitions never lost through coalescing |
| P2 | `VideoAssembler::add` copies unit payloads and `finish` allocates assembly/FEC buffers | Pool bounded packet/assembly storage; avoid redundant copies; keep resource ceilings | Allocation count and CPU time drop on identical replay; fuzz/resource tests remain valid |
| P2 | FEC reconstructs matrices and loops over byte arithmetic | Benchmark matrix cache keyed by dimensions/erasure pattern; bounded SIMD-dispatch experiment | Bit-identical recovery versus scalar path; bounded cache; wins on real loss patterns |
| P2 | Scene extraction reads each ML scalar through Objective-C indexed access | Bulk typed/strided extraction after dtype/stride validation | Embeddings numerically match and inference overhead falls |
| P2 | Shader/texture memory can be large; current ring permits up to 1.5 GiB | Backend/device memory budgets, pooled textures, resolution-aware allocation planner | Mobile allocation failures recover gracefully; memory plateaus over soak |

Do not misreport the visible `waitUntilCompleted` in `frame_to_texture` as a production per-frame stall: that helper is explicitly exposed for tests; the renderer also waits during teardown. The decoder wait is the concrete steady-state investigation target. Similarly, the audio callback already uses `try_lock`; the opportunity is avoiding silence on contention and improving observability, not fixing a blocking mutex that is not there.

At 1080p, one BGRA surface is approximately 7.9 MiB; six are approximately 47.5 MiB before decode/flow/upscale resources. A 4K BGRA surface is about 31.6 MiB. Calculate bytes per format, size, level, history count and in-flight count before allocation. Initial engineering budgets: ≤128 MiB of render working textures for a 1080p mobile baseline and ≤256 MiB desktop baseline, excluding separately reported decoder/ML/webview allocations. These are design targets to validate, not current measurements; lower-tier devices may need tighter limits and disabled FG.

## 11. Audio, storage, controllers and frontend integration

Portable audio reuses header parsing but adds a vetted Opus implementation with malformed-packet limits, PLC and optional FEC as supported by actual stream semantics. Expose PCM format, channel layout, clock and underrun metrics. WASAPI, Linux output, Oboe and Apple output share jitter/drift policy while retaining native route/lifecycle logic. Oboe is specifically intended for Android low-latency audio; measure actual route latency rather than assuming its use guarantees it. [Oboe](https://developer.android.com/games/sdk/oboe).

Storage must use per-user application-data/cache directories on desktop and app-private directories on mobile. Preserve portable mode as an explicit desktop option; do not mutate signed mobile application bundles. Move credentials into OS-backed protected storage and keep ordinary preferences in versioned files. Implement Windows ACL/atomic-replace semantics rather than pretending Unix 0600 is portable. Migration copies and verifies existing data before switching paths, avoids logging secrets, and leaves a rollback path. Account sign-out must clear all corresponding stores. Model/cache cleanup must not delete registrations or keymaps.

Physical controllers are essential especially on TV. Keep `PadSource`/`PadSink` and shared report parsing. Add Windows/Linux/Android/iOS adapters, capability-gating gyro, touchpad, battery, adaptive triggers, rumble and high-rate haptics. Standard mobile game-controller APIs do not guarantee raw DualSense feature parity. Report unsupported extras and retain basic buttons/sticks. Arbitration between keyboard/mouse, touch and pad must reset old state and avoid concurrent ownership fighting. Touch fallback provides basic play and a release menu where hardware input is absent.

Settings v2 separates graphics backend, decoder preference, upscaler, frame-generation mode/multiplier, quality/power policy and scene model tier. Map old `engine=fsr`, `interpolator`, `frame_mode` and legacy frame-generation encoding into new typed enums without losing user preferences. Persist requested settings, compute effective settings and an explanatory reason code per unavailable option. `Auto` should recover to a working backend; an explicitly forced backend should explain failure and offer a switch, never silently lie about which backend is active. Surface stats show real/unique/generated/presented FPS separately and expose queue age, copy route, GPU timing availability and thermal fallback.

## 12. Validation targets and release gates

Use proposed targets as acceptance thresholds after establishing baseline hardware classes; do not present them as results. For a clean 1080p60 local replay on reference desktop/mobile hardware: decode-to-present submission p95 ≤25 ms in non-FG mode, decoded mailbox age p95 ≤1 source interval, present deadline misses <1% over 10 minutes, A/V offset magnitude ≤40 ms for a wired/local output route, no steadily growing queue/memory. Smooth FG's intentional lookahead is reported separately. Capture-release-to-neutral output should be ≤one mapper tick plus scheduling budget, with p95 local input processing ≤8 ms on the reference device. Physical console input-to-photon is measured independently and includes network/console latency.

Scene provisional target: held-out macro-F1 ≥0.90 and ≤0.1 false mapping switches/minute, with p95 action-associated switch delay ≤1.5 seconds where labels indicate an unambiguous transition. Preserve current behavior if a new model fails the accuracy gate. Report difficult/OOD games separately; never hide failure behind aggregate accuracy. Runtime safety gates are absolute: zero old-epoch publications, bounded queues, no stuck input on capture loss, no stale GPU resources, and explicit unsupported states.

Test layers:

1. Portable unit/property/replay tests for timestamps, ID wrap, NAL/loss recovery, color math, filter distributions, event normalization, settings migration, queue accounting and allocation caps.
2. Decoder conformance clips: H.264/HEVC supported profiles, parameter changes, corrupt/truncated NALs, loss, reordering, 30/60 fps, repeated frames, odd crop/stride and reset. Unsupported profiles fail cleanly.
3. GPU conformance: golden spatial filters, color bars/range, motion vectors/occlusion, cut/fade/flash sequences, GL fallback, device loss, resize, native texture lease stress, mixed adapters and CPU-copy path. Compare tolerances, not exact floating-point bytes across vendors.
4. End-to-end mock-console tests on each platform, with loopback/local replay independent of unavailable external assets. Test stalled consumer, disconnect while decoding, lost display and audio interruption.
5. Real PS4 and PS5 LAN/WAN qualification with documented firmware/network conditions, pairing/authentication, wake, loss recovery and latency. The README's current mock-only status cannot satisfy this gate.
6. Physical input/orientation matrix: phones/tablets/TV, USB/Bluetooth, multiple layouts, 20-minute continuous movement without pointer edge saturation, rapid focus/modal transitions, locked screen, device unplug, portrait device rotation, foldables and resizable windows.
7. Soak: 30 minutes on mobile/TV for thermals and 2 hours desktop; no unbounded growth, route/device hotplug, capture neutrality, crash-free reconnect loops. Collect p50/p95/p99, hardware/OS/backend/driver/model hash and flags.

CI adds Rust fmt/clippy/portable tests, frontend `node --test tests/*.test.js`, native desktop builds, Android ABI builds and iOS simulator/device compile checks on appropriate hosts. Hardware jobs are separate from software-only CI; emulator/simulator success is not proof of hardware decode or input lock. Record SDK/NDK/Xcode/Rust versions and dependency locks. Model parity and GPU golden suites can run on scheduled qualified hosts, with small fixtures in ordinary CI.

## 13. Rollout and dependency order

**Phase 0 — baseline and feasibility:** establish runnable replay assets/metrics; inventory compile blockers; prototype Windows texture interop, Android processable decode surface, iOS native view and mouse lock. These spikes have explicit go/no-go outputs. No full backend rewrite before the surface/interop risk is tested.

**Phase 1 — contracts and regression protection:** introduce media/render/input contracts, settings capabilities and storage abstraction; extract pure policies with compatibility re-exports; build CI. Keep macOS production behavior unchanged except separately tested fixes.

**Phase 2 — playable desktop:** Windows D3D12 and Linux Vulkan complete decode/audio/input/session flows with native/bilinear rendering. Add GLES/GL compatibility and spatial upscale. Validate PS4/PS5 before performance marketing.

**Phase 3 — playable mobile and TV:** Android MediaCodec/audio/lifecycle, iOS VideoToolbox/Metal/audio, native capture and landscape handling, TV remote flows, native authentication/storage. Basic playback and capture correctness precede optional effects.

**Phase 4 — visual and semantic quality:** shared spatial filter parity, frame generation and cut/reset logic, portable scene inference, evaluation/calibration and mobile model tier. Integrate thermal budgets and end-to-end latency policy.

**Phase 5 — qualification and release:** full device matrix, GPU/decoder fallback, prolonged sessions, packaging/signing, self-contained docs, licensing/model provenance review and release artifacts. Publish a capability table listing tested devices/backends and exclusions.

A practical staffing assumption is 3–5 engineers with desktop GPU/media, mobile/native and ML/test expertise. A preliminary planning envelope is 20–36 calendar weeks with overlapping workstreams; this is not a delivery commitment. Interop spikes, hardware availability and real-console findings determine the revised estimate. Single-engineer execution will take substantially longer. Track task estimates as small (2–4 days), medium (1–2 weeks), large (2–4 weeks), or research (time-boxed 1 week then decision); integration and device qualification are additional, not hidden inside shader estimates.

Definition of done for each task: reviewed code at named seams, targeted regression tests, actual capability/failure behavior, documentation updates and evidence artifact. Definition of done for a platform: playable real-console stream with audio and input, correct lifecycle, qualified packaging and tested fallback paths. Definition of done for FG: quality and latency reports plus runtime disable behavior, not merely a higher FPS counter.

## 14. Decisions that remain empirical

- Choose/pin shared GPU library only after native decoder texture import tests. If public wgpu APIs cannot express a required import safely, isolate version-pinned HAL/native interop or use the native backend; do not spread unsafe imports across application code.
- Verify whether iPhone/iPad target OS/device combinations actually provide the requested physical mouse capture. Native API declarations alone are insufficient evidence.
- Establish the oldest Android TV hardware worth supporting and whether armv7 is required; test sustained memory/thermal limits before shipping optional ML.
- Decide model distribution based on actual available weights, reproducible exports and license inventory. No inferred runtime can compensate for absent model assets.
- Select decode/software dependency licensing and packaging with maintainers before distribution. Fix contradictory README claims and missing root license rather than assuming third-party icons' license covers the project.
- Confirm real-console protocol behavior, supported stream formats, remote authentication and clocks. Keep mock and real evidence labeled separately.

## 15. Code navigation and tracking

The accompanying task appendix provides implementation checklists, specific existing/proposed paths, dependencies, validation and rollback for each work package. GitHub task links are added after creation. The audit map links to exact source locations in the inspected commit; subsequent implementation must rebase line references against current main.

### Verification performed during this audit

The existing frontend suite was run on the inspected checkout: `node --test tests/*.test.js` — **31 passed, 0 failed**. These are existing source/UI assertions, not cross-platform runtime qualification. No Rust build, GPU benchmark, model inference run or real-console test was performed.


## Implementation issue index

Each issue includes pinned source locations, implementation checklists, dependencies, acceptance criteria, and rollback/fallback behavior. Phase is the primary delivery phase; CI and qualification continue throughout.

- [ ] #2 — **T01 / P0 / phase 0**: Establish replay fixtures, performance baselines and hardware qualification matrix
- [ ] #3 — **T02 / P0 / phase 1**: Remove platform compile blockers and introduce target-specific dependency boundaries
- [ ] #4 — **T03 / P0 / phase 1**: Define owned media frames, decoder interfaces and epoch-aware timestamps
- [ ] #5 — **T04 / P0 / phase 1**: Extract portable render timing, camera and adaptive-quality policy
- [ ] #6 — **T05 / P0 / phase 0**: Prototype decoder-to-GPU interoperability before committing backend design
- [ ] #7 — **T06 / P0 / phase 2**: Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets
- [ ] #8 — **T07 / P1 / phase 2**: Add explicit OpenGL and OpenGL ES compatibility rendering
- [ ] #9 — **T08 / P0 / phase 2**: Implement Windows Media Foundation decode and native gameplay host
- [ ] #10 — **T09 / P0 / phase 2**: Implement Linux decode, Wayland/X11 surfaces and distribution integration
- [ ] #11 — **T10 / P0 / phase 2**: Port Opus decoding and audio output with independent A/V clock policy
- [ ] #12 — **T11 / P0 / phase 2**: Extract portable input host and implement Windows/Linux capture adapters
- [ ] #13 — **T12 / P0 / phase 3**: Build Android native shell, MediaCodec surfaces and lifecycle integration
- [ ] #14 — **T13 / P0 / phase 3**: Implement Android pointer lock, physical keyboard mapping and fixed-landscape gameplay
- [ ] #15 — **T14 / P0 / phase 3**: Deliver Android TV launcher, remote-first UX and low-resource qualification
- [ ] #16 — **T15 / P0 / phase 3**: Implement iOS/iPadOS native media host, Metal and sandbox lifecycle
- [ ] #17 — **T16 / P0 / phase 3**: Implement iOS/iPadOS keyboard, relative mouse capture and landscape controller
- [ ] #18 — **T17 / P1 / phase 4**: Port spatial upscaling and color pipeline across GPU backends
- [ ] #19 — **T18 / P1 / phase 4**: Port video-derived frame generation with cuts, confidence and backend parity
- [ ] #20 — **T19 / P1 / phase 4**: Implement display-driven pacing, thermal budgets and end-to-end latency telemetry
- [ ] #21 — **T20 / P1 / phase 4**: Make scene inference portable and model artifacts reproducible
- [ ] #22 — **T21 / P0 / phase 1**: Prevent stale scene publication and add inference recovery/freshness state
- [ ] #23 — **T22 / P1 / phase 4**: Harden scene probe validation and calibrate mapping decisions
- [ ] #24 — **T23 / P0 / phase 3**: Add controller backends, touch fallback and input-source arbitration
- [ ] #25 — **T24 / P1 / phase 0**: Build game-disjoint scene evaluation with real action traces
- [ ] #26 — **T25 / P2 / phase 4**: Develop a mobile scene-model tier and adaptive sampling policy
- [ ] #27 — **T26 / P1 / phase 4**: Optimize decode scheduling, texture copies and duplicate detection from measured traces
- [ ] #28 — **T27 / P2 / phase 4**: Optimize transport queues, assembly allocations and FEC without changing protocol behavior
- [ ] #29 — **T28 / P0 / phase 1**: Implement native storage, credential protection and migration-safe app paths
- [ ] #30 — **T29 / P1 / phase 3**: Add capability-driven settings, mobile/TV layouts and truthful diagnostics
- [ ] #31 — **T30 / P0 / phase 1**: Add cross-platform CI, GPU/media conformance and hardware release qualification
- [ ] #32 — **T31 / P1 / phase 5**: Package, sign and document platform releases and model/license provenance
- [ ] #33 — **T32 / P2 / phase 5**: Evaluate optional Vulkan-over-Metal and advanced temporal reconstruction feasibility

Start with #2 (baseline), #6 (interop spikes), and #25 (scene evaluation); then #3–#5 establish the portable boundaries. Native input feasibility for iOS/Android should be proven early within #14 and #17 before full platform implementation. Dependencies in individual issues are the authoritative sequencing links.


# Appendix A — Complete implementation work packages

GitHub tracking parent: [Spec / Epic #1](https://github.com/Gourav1100/openps-rs/issues/1). New paths are proposals; existing source links are pinned to the audit commit.


---

## T01: Establish replay fixtures, performance baselines and hardware qualification matrix

**GitHub:** [Issue #2](https://github.com/Gourav1100/openps-rs/issues/2)

Parent specification: #1

**Task:** T01 · **Priority:** P0 · **Phase:** 0 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Current README declares mock-only console validation; optional real-clip tests depend on external assets. New performance claims need reproducible baselines.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-testkit/src/stream.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-testkit/src/stream.rs#L1)
- [crates/openps-demo/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-demo/src/lib.rs#L1)
- [crates/openps-metal/tests/stage_cost.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/stage_cost.rs#L1)
- [crates/openps-stream/tests/perf.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-stream/tests/perf.rs#L1)
- [README.md:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/README.md#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Inventory existing test and capture assets, their provenance and missing model/media downloads.
- [ ] Create short redistributable or generated H.264/HEVC replay fixtures with loss, duplication, variable cadence and reconnect annotations.
- [ ] Record reference hardware, OS, driver, display refresh, codec/profile, resolution and backend for every result.
- [ ] Capture current macOS CPU/GPU stage timing, queue age, input processing and A/V offset before refactoring.
- [ ] Define device classes for Windows/Linux, Android phone/tablet/TV and iPhone/iPad; identify actual PS4/PS5 test access.

### Dependencies

No implementation dependencies; start with the audited baseline.

### Acceptance criteria

- [ ] A clean checkout can run portable replay without sibling analysis directories.
- [ ] Baseline reports identify measured versus simulated data and include p50/p95/p99 where meaningful.
- [ ] Missing hardware/assets are explicit blockers, never recorded as passing tests.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Fixtures and instrumentation are additive; keep diagnostics opt-in and remove overhead from normal frame loops.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T02: Remove platform compile blockers and introduce target-specific dependency boundaries

**GitHub:** [Issue #3](https://github.com/Gourav1100/openps-rs/issues/3)

Parent specification: #1

**Task:** T02 · **Priority:** P0 · **Phase:** 1 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Non-macOS branches use NullMedia and unguarded Metal types; Unix-specific private file operations block Windows compilation.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/Cargo.toml#L1)
- [src-tauri/Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/Cargo.toml#L1)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [src-tauri/src/paths.rs:25](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/paths.rs#L25)
- [crates/openps-core/src/store.rs:255](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-core/src/store.rs#L255)
- [crates/openps-psn/src/store.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-psn/src/store.rs#L1)
- [crates/openps-metal/Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/Cargo.toml#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Build an explicit target/feature inventory including demo and psn-live.
- [ ] Move portable types out of macOS-only dependencies; gate framework linkage and native modules at the narrowest layer.
- [ ] Route file creation/replacement through a platform storage abstraction.
- [ ] Audit all build.rs files and Apple desktop APIs before enabling iOS.
- [ ] Add portable compile targets first, then compile the shell with stubs that report unavailable capabilities honestly.

### Dependencies

- T01: #2 — Establish replay fixtures, performance baselines and hardware qualification matrix

### Acceptance criteria

- [ ] Portable workspace crates compile on Windows/Linux target hosts.
- [ ] Non-Apple builds do not link Objective-C/Apple frameworks.
- [ ] macOS tests/build remain unchanged; stub builds are not marketed as playable.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Use compatibility re-exports and small commits; preserve existing macOS adapter until replacement is qualified.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T03: Define owned media frames, decoder interfaces and epoch-aware timestamps

**GitHub:** [Issue #4](https://github.com/Gourav1100/openps-rs/issues/4)

Parent specification: #1

**Task:** T03 · **Priority:** P0 · **Phase:** 1 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Concrete VT frame types and id*1500 timestamp encoding couple decoder, renderer and scene inference to one platform.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)
- [crates/openps-video/src/media.rs:16](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/media.rs#L16)
- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-core/src/events.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-core/src/events.rs#L1)
- [crates/openps-stream/src/video.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-stream/src/video.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add proposed openps-media types for frame identity, stream epoch, estimated/source timestamps, format/color/crop and native leases.
- [ ] Introduce asynchronous decoder contract and typed error/recovery states while adapting existing VT code.
- [ ] Unwrap wire sequence IDs once; preserve original frame identity independently of decoder PTS.
- [ ] Document synchronization and release-thread ownership for every native frame variant.
- [ ] Bound decode/completion queues and ignore callbacks from obsolete epochs.

### Dependencies

- T02: #3 — Remove platform compile blockers and introduce target-specific dependency boundaries

### Acceptance criteria

- [ ] Tests cover wraparound, 30/60 fps, restart, format change, late callback, loss and shutdown.
- [ ] No native resource can recycle while the GPU or scene tap retains it.
- [ ] Existing IDR throttling and parameter-set behavior remain covered.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep an adapter for the old VT call sites until all consumers migrate; do not change wire protocol timing in this task.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T04: Extract portable render timing, camera and adaptive-quality policy

**GitHub:** [Issue #5](https://github.com/Gourav1100/openps-rs/issues/5)

Parent specification: #1

**Task:** T04 · **Priority:** P0 · **Phase:** 1 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Reusable scheduling and camera policy currently live inside an Apple-dependent crate and leak into shell signatures.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-metal/src/timeline.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/timeline.rs#L1)
- [crates/openps-metal/src/ladder.rs:22](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/ladder.rs#L22)
- [crates/openps-metal/src/look.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/look.rs#L1)
- [crates/openps-metal/src/camera.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/camera.rs#L1)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Create proposed openps-render crate and mechanically move timeline, ladder, camera and SharedLook with compatibility exports.
- [ ] Move settings/statistics contracts after removing native resource types.
- [ ] Keep Metal resource creation and presentation in openps-metal.
- [ ] Add platform clock/display capability inputs instead of NSScreen assumptions.
- [ ] Document that actual ladder STEP_HZ is 5 although README says 15.

### Dependencies

- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps

### Acceptance criteria

- [ ] Existing timeline/replay/camera/ladder tests run against extracted code on non-Apple hosts.
- [ ] No policy behavior changes in extraction PRs.
- [ ] macOS before/after replay output and timing counters remain equivalent within declared tolerance.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Re-export old paths for a migration window and keep behavior changes in separate PRs.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T05: Prototype decoder-to-GPU interoperability before committing backend design

**GitHub:** [Issue #6](https://github.com/Gourav1100/openps-rs/issues/6)

Parent specification: #1

**Task:** T05 · **Priority:** P0 · **Phase:** 0 · **Estimate:** R (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Native decoder outputs are not automatically importable by a generic GPU library; this is the largest architecture risk.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Create isolated proposed workbench examples for D3D11 MFT to D3D12, Linux VA-API/DMA-BUF to Vulkan, and Android MediaCodec to GLES/AHardwareBuffer Vulkan.
- [ ] Check adapter identity, pixel formats, format modifiers, external synchronization and producer-surface compatibility.
- [ ] Measure zero-copy versus application-owned one-copy textures, including decoder pool retention.
- [ ] Evaluate pinned wgpu public/HAL APIs versus small native interop modules; record unsafe ownership proofs.
- [ ] Produce ADR with supported paths, fallback selection, tested hardware and explicit failed cases.

### Dependencies

- T01: #2 — Establish replay fixtures, performance baselines and hardware qualification matrix

### Acceptance criteria

- [ ] Each prototype displays a known clip with correct color and survives repeated resize/teardown.
- [ ] No unsupported path is marked zero-copy.
- [ ] ADR gives a go/no-go decision and library versions for each shipping backend; failed optional import does not block basic playback.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Examples do not replace production rendering; select copy fallback or a native backend when safe import cannot be demonstrated.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T06: Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets

**GitHub:** [Issue #7](https://github.com/Gourav1100/openps-rs/issues/7)

Parent specification: #1

**Task:** T06 · **Priority:** P0 · **Phase:** 2 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

New platforms need real presentation and processing, not only protocol support.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Create proposed openps-render-wgpu with native surface creation, import/upload paths and baseline color conversion/presentation.
- [ ] Implement feature/limit enumeration, adapter selection, explicit backend selection and failure reason codes.
- [ ] Use bounded command submission and native synchronization from T05; cache pipelines and pool resources.
- [ ] Handle crop/stride, full/limited range, SDR color metadata, resize/minimize/device loss.
- [ ] Add resource planner and distinguish texture working-set from decoder/model allocations.

### Dependencies

- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T04: #5 — Extract portable render timing, camera and adaptive-quality policy
- T05: #6 — Prototype decoder-to-GPU interoperability before committing backend design

### Acceptance criteria

- [ ] Windows D3D12 and Linux/Android Vulkan qualified paths display replay correctly.
- [ ] Device loss and resize never retain stale surfaces; allocation failures fall back cleanly.
- [ ] Stats report actual backend, import/copy route, limits and queue age.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Preserve native Metal and allow native/bilinear or copy-upload mode if advanced processing fails.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T07: Add explicit OpenGL and OpenGL ES compatibility rendering

**GitHub:** [Issue #8](https://github.com/Gourav1100/openps-rs/issues/8)

Parent specification: #1

**Task:** T07 · **Priority:** P1 · **Phase:** 2 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

GL support must include actual native context/external-texture handling and must not imply uniform compute capability.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [src-tauri/Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/Cargo.toml#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Create proposed openps-render-gl with desktop GL context and EGL/GLES implementations; document exact minimum feature sets.
- [ ] Implement upload and YUV/external-OES paths, fragment spatial scaling and presentation.
- [ ] Keep capability tiers for basic video, spatial filters and compute motion generation separate.
- [ ] On Windows distinguish native OpenGL from ANGLE translation in UI/diagnostics.
- [ ] Add fallback ordering and explicit errors for forced unavailable backends.

### Dependencies

- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T04: #5 — Extract portable render timing, camera and adaptive-quality policy
- T05: #6 — Prototype decoder-to-GPU interoperability before committing backend design

### Acceptance criteria

- [ ] A no-compute device can display video and use qualified spatial upscale.
- [ ] GL/GLES conformance images match reference tolerances.
- [ ] Context recreation, focus, resize and thread ownership tests pass on real drivers.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Fail optional passes independently; retain basic rendering and expose why FG is unavailable.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T08: Implement Windows Media Foundation decode and native gameplay host

**GitHub:** [Issue #9](https://github.com/Gourav1100/openps-rs/issues/9)

Parent specification: #1

**Task:** T08 · **Priority:** P0 · **Phase:** 2 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Windows currently has no usable media sink or native video host.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)
- [crates/openps-video/src/nal.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/nal.rs#L1)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add proposed decoder/windows.rs for hardware MFT enumeration, configuration, asynchronous decode and flush/rebuild.
- [ ] Use D3D11 device-manager integration and the tested shared/copy path to rendering.
- [ ] Handle H.264/HEVC availability, parameter-set updates, device removal and unsupported profiles.
- [ ] Add proposed platform/windows.rs child-window/composition host with WebView2 overlay, DPI/fullscreen/monitor transitions.
- [ ] Integrate stream lifecycle and report fatal decoder initialization rather than silently using NullMedia.

### Dependencies

- T02: #3 — Remove platform compile blockers and introduce target-specific dependency boundaries
- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T05: #6 — Prototype decoder-to-GPU interoperability before committing backend design
- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets

### Acceptance criteria

- [ ] Mock and real-console H.264 streams play with audio/input once dependent integrations land; HEVC is qualified or gracefully negotiated down.
- [ ] Tests cover resolution changes, decoder failure, repeated connect/disconnect and minimize.
- [ ] D3D12 and Vulkan selection use verified import/fallback paths.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Offer software decode/native resolution where qualified; otherwise report a clear codec error and preserve app navigation.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T09: Implement Linux decode, Wayland/X11 surfaces and distribution integration

**GitHub:** [Issue #10](https://github.com/Gourav1100/openps-rs/issues/10)

Parent specification: #1

**Task:** T09 · **Priority:** P0 · **Phase:** 2 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Linux requires native media, compositor-specific surfaces and controlled codec/runtime dependencies.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [scripts/build.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/build.sh#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add proposed decoder/linux.rs around a narrowly configured FFmpeg/libavcodec wrapper.
- [ ] Establish software decode first and add VA-API export/import with modifiers, stride and synchronization checks.
- [ ] Qualify Intel/AMD and document NVIDIA hardware or software paths rather than inferring support.
- [ ] Add proposed platform/linux.rs for Wayland and X11 lifecycle/composition.
- [ ] Define supported distro/library baseline and package dependencies, sandbox GPU/audio access and read-only installation behavior.

### Dependencies

- T02: #3 — Remove platform compile blockers and introduce target-specific dependency boundaries
- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T05: #6 — Prototype decoder-to-GPU interoperability before committing backend design
- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets
- T07: #8 — Add explicit OpenGL and OpenGL ES compatibility rendering

### Acceptance criteria

- [ ] Wayland and X11 complete replay/session flows with Vulkan and a GL fallback.
- [ ] Decoder/driver absence produces a working fallback or actionable error.
- [ ] Test default audio/display change, suspend/resume and compositor resize without crashes.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep software decode/upload as a diagnostic fallback with honest performance limits.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T10: Port Opus decoding and audio output with independent A/V clock policy

**GitHub:** [Issue #11](https://github.com/Gourav1100/openps-rs/issues/11)

Parent specification: #1

**Task:** T10 · **Priority:** P0 · **Phase:** 2 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Audio is currently Apple-only; silent clients are not playable platform ports.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-audio/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/lib.rs#L1)
- [crates/openps-audio/src/opus.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/opus.rs#L1)
- [crates/openps-audio/src/player.rs:88](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/player.rs#L88)
- [crates/openps-audio/src/media.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/media.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Separate portable Opus/PLC/jitter/drift policy from native audio output.
- [ ] Add WASAPI, Linux and Oboe adapters; retain/adapt Apple playback with AVAudioSession on iOS.
- [ ] Use preallocated PCM ring and nonblocking allocation-free callbacks; count contention, underrun, overrun and route latency.
- [ ] Bound jitter buffering and add measured clock-drift correction rather than repeated large trims.
- [ ] Handle audio focus/interruption, sample-rate/channel changes, hotplug and device loss.

### Dependencies

- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps

### Acceptance criteria

- [ ] Golden Opus decode and malformed-packet tests pass.
- [ ] A/V offset and callback deadlines meet defined reference-device targets.
- [ ] Bluetooth/HDMI route latency is measured separately, not hidden in nominal buffer settings.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Retain conservative buffer profiles and existing Apple output until new output passes soak tests.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T11: Extract portable input host and implement Windows/Linux capture adapters

**GitHub:** [Issue #12](https://github.com/Gourav1100/openps-rs/issues/12)

Parent specification: #1

**Task:** T11 · **Priority:** P0 · **Phase:** 2 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

The mapping engine is portable, but its runner, input collection and cursor capture are tied to macOS.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [crates/openps-input/src/mapping.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/mapping.rs#L1)
- [crates/openps-macinput/src/hub.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-macinput/src/hub.rs#L1)
- [crates/openps-macinput/src/runner.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-macinput/src/runner.rs#L1)
- [src-tauri/src/input.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/input.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add proposed openps-input-host for physical key normalization, bounded delta accumulation and capture state machine.
- [ ] Keep engine::Platform, separating in-app capture from target-app management operations.
- [ ] Implement Windows Raw Input/focus/confinement and Linux Wayland relative-pointer/constraints plus X11 raw capture.
- [ ] Prevent native/DOM double delivery and separate text-entry/IME from gameplay scan codes.
- [ ] Reset held state, mouse deltas and output pad on release, disconnect and focus loss.

### Dependencies

- T02: #3 — Remove platform compile blockers and introduce target-specific dependency boundaries
- T04: #5 — Extract portable render timing, camera and adaptive-quality policy

### Acceptance criteria

- [ ] Tests cover repeat, modifiers, layouts, wheel/buttons, reconnect and input-source arbitration.
- [ ] 20-minute movement never hits a pointer edge on qualified capture paths.
- [ ] Unsupported Wayland capture is reported, with working release/navigation.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep macinput as an adapter; fall back to released UI mode rather than partially captured gameplay.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T12: Build Android native shell, MediaCodec surfaces and lifecycle integration

**GitHub:** [Issue #13](https://github.com/Gourav1100/openps-rs/issues/13)

Parent specification: #1

**Task:** T12 · **Priority:** P0 · **Phase:** 3 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

A mobile entry-point attribute and Android config stub do not implement native playback or lifecycle.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [src-tauri/tauri.conf.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/tauri.conf.json#L1)
- [src-tauri/Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/Cargo.toml#L1)
- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Create reproducible Android generated project and proposed local platform plugin in Kotlin/Rust.
- [ ] Host native surface with verified WebView composition and direct native input/decoder bridge.
- [ ] Implement MediaCodec async decode, processable versus direct-display surfaces, output crop and low-latency capability probing.
- [ ] Handle surface destruction, activity recreation, memory pressure, audio focus, screen-off and network changes.
- [ ] Build supported ABIs and validate native library/page-size packaging with pinned SDK/NDK.

### Dependencies

- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T05: #6 — Prototype decoder-to-GPU interoperability before committing backend design
- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets
- T07: #8 — Add explicit OpenGL and OpenGL ES compatibility rendering
- T10: #11 — Port Opus decoding and audio output with independent A/V clock policy

### Acceptance criteria

- [ ] Android phone/tablet replays H.264/qualified HEVC with audio and selectable Vulkan/GLES or direct surface fallback.
- [ ] Repeated pause/resume and surface replacement do not leak, deadlock or publish stale frames.
- [ ] Unsupported postprocessing surface modes produce explicit effective settings.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Direct-display MediaCodec path remains available; switching surface type uses a controlled decoder restart.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T13: Implement Android pointer lock, physical keyboard mapping and fixed-landscape gameplay

**GitHub:** [Issue #14](https://github.com/Gourav1100/openps-rs/issues/14)

Parent specification: #1

**Task:** T13 · **Priority:** P0 · **Phase:** 3 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Requested mouse lock and landscape behavior must be native, focus-aware and robust to OS windowing overrides.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [src-tauri/src/input.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/input.rs#L1)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)
- [dist/style.css:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/style.css#L1)
- [src-tauri/tauri.conf.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/tauri.conf.json#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add requestPointerCapture/onPointerCaptureChange/captured-event handling to the native gameplay view.
- [ ] Forward physical key transitions to shared input host; preserve IME/UI editing and prevent duplicate DOM input.
- [ ] Implement capture states, visible release button and neutral-pad/reset on capture loss.
- [ ] Request landscape orientation and maintain a landscape viewport with letterboxing when OS window bounds override it.
- [ ] Transform touch/pointer coordinates after resize, fold/unfold and density changes.

### Dependencies

- T11: #12 — Extract portable input host and implement Windows/Linux capture adapters
- T12: #13 — Build Android native shell, MediaCodec surfaces and lifecycle integration

### Acceptance criteria

- [ ] USB/Bluetooth keyboard and mouse pass sustained relative movement, unplug/reconnect, modifiers and release tests.
- [ ] Phone rotation cannot turn gameplay into portrait content.
- [ ] Android 16/17 large-screen and desktop-window behavior is documented from physical/emulator tests; system gestures still work.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

If capture fails, show released/unavailable state and retain touch/controller navigation; never fake locked state.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T14: Deliver Android TV launcher, remote-first UX and low-resource qualification

**GitHub:** [Issue #15](https://github.com/Gourav1100/openps-rs/issues/15)

Parent specification: #1

**Task:** T14 · **Priority:** P0 · **Phase:** 3 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Android TV needs a complete D-pad-first product flow and different performance defaults from phones.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [dist/index.html:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/index.html#L1)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)
- [dist/style.css:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/style.css#L1)
- [src-tauri/tauri.conf.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/tauri.conf.json#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add Leanback launcher, banner and touchscreen-not-required declarations to reproducible manifest customization.
- [ ] Implement TV-specific focus graph, visible focus, Back behavior, large targets and overscan-safe layout.
- [ ] Make pairing/authentication/settings/release/disconnect possible without mouse, touch or F-keys.
- [ ] Default to native resolution/no FG and conservative inference until sustained-load qualification.
- [ ] Test HDMI hotplug, 59.94/60 Hz, sleep, Bluetooth reconnect and low-memory dongles.

### Dependencies

- T12: #13 — Build Android native shell, MediaCodec surfaces and lifecycle integration
- T13: #14 — Implement Android pointer lock, physical keyboard mapping and fixed-landscape gameplay
- T23: #24 — Add controller backends, touch fallback and input-source arbitration

### Acceptance criteria

- [ ] All onboarding and gameplay-management flows work with a basic remote.
- [ ] Actual TV hardware streams audio/video and recovers from sleep and HDMI route changes.
- [ ] Device capability report distinguishes panel resolution from decode/processing limits.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Reuse Android basic media mode; optional effects disable independently without breaking navigation.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T15: Implement iOS/iPadOS native media host, Metal and sandbox lifecycle

**GitHub:** [Issue #16](https://github.com/Gourav1100/openps-rs/issues/16)

Parent specification: #1

**Task:** T15 · **Priority:** P0 · **Phase:** 3 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Apple media APIs offer reuse, but current AppKit surfaces, framework guards and storage paths are not iOS-compatible.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-video/build.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/build.rs#L1)
- [crates/openps-metal/build.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/build.rs#L1)
- [crates/openps-audio/build.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/build.rs#L1)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Create reproducible Apple generated project and Swift/native platform plugin.
- [ ] Adapt VideoToolbox/Metal to UIView/CAMetalLayer and per-window scene timing; remove NSView/NSScreen assumptions.
- [ ] Availability-gate MetalFX and retain built-in/native rendering fallback.
- [ ] Integrate AVAudioSession, foreground/background, interruption and surface cancellation with epochs.
- [ ] Handle local-network access, manual console addressing, sign-in callback and sandbox directories.

### Dependencies

- T02: #3 — Remove platform compile blockers and introduce target-specific dependency boundaries
- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T04: #5 — Extract portable render timing, camera and adaptive-quality policy
- T10: #11 — Port Opus decoding and audio output with independent A/V clock policy

### Acceptance criteria

- [ ] Physical iPhone and iPad stream known H.264/HEVC clips and real-console content with audio.
- [ ] Suspend/resume, interruptions, memory pressure and view replacement do not retain obsolete GPU/decoder state.
- [ ] Device and simulator compile checks are separate from actual hardware qualification.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep optional MetalFX/inference off on unsupported devices; native VideoToolbox/Metal playback is the baseline.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T16: Implement iOS/iPadOS keyboard, relative mouse capture and landscape controller

**GitHub:** [Issue #17](https://github.com/Gourav1100/openps-rs/issues/17)

Parent specification: #1

**Task:** T16 · **Priority:** P0 · **Phase:** 3 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

UIKit pointer-lock preference and GameController input need native integration and per-device verification; desktop cursor warping is not portable.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [src-tauri/src/input.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/input.rs#L1)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)
- [dist/style.css:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/style.css#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add GCKeyboard/GCMouse connection and event handlers, normalized through the shared input host.
- [ ] Implement prefersPointerLocked/update hooks in the gameplay view controller and report observed capture/focus state.
- [ ] Add release control and neutralize held state on modal, scene deactivation, mouse disconnect and session close.
- [ ] Declare landscape orientations and availability-gated scene lock preferences; support resized iPad windows with landscape content.
- [ ] Validate iPhone and iPad separately and expose unsupported hardware/OS combinations honestly.

### Dependencies

- T11: #12 — Extract portable input host and implement Windows/Linux capture adapters
- T15: #16 — Implement iOS/iPadOS native media host, Metal and sandbox lifecycle

### Acceptance criteria

- [ ] Qualified hardware passes sustained relative movement, keyboard transitions, release/reacquire and no-camera-jump tests.
- [ ] System modals and navigation can release capture without stuck input.
- [ ] Landscape behavior is documented for fullscreen and resizable windows; UIRequiresFullScreen is not the sole mechanism.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Fallback to controller/touch or released pointer navigation when native capture is unavailable; do not claim universal iPhone lock.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T17: Port spatial upscaling and color pipeline across GPU backends

**GitHub:** [Issue #18](https://github.com/Gourav1100/openps-rs/issues/18)

Parent specification: #1

**Task:** T17 · **Priority:** P1 · **Phase:** 4 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Existing EASU/RCAS and finishing effects must retain quality across Vulkan, DirectX, GL/GLES and Metal.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/tests/fsr_compare.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/fsr_compare.rs#L1)
- [crates/openps-metal/tests/video_quality.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/video_quality.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Write backend-neutral reference math for EASU/RCAS, sharpness, bounds and color conversions.
- [ ] Port kernels to shared WGSL and qualified GLSL/fragment variants with pipeline caching.
- [ ] Prefer NV12/P010 conversion on GPU; make SDR range/matrix and linear-light boundaries explicit.
- [ ] Clamp requested scale to drawable size and memory/texture limits; provide native/bilinear fallback.
- [ ] Add HDR only behind separately tested color/format capabilities.

### Dependencies

- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets
- T07: #8 — Add explicit OpenGL and OpenGL ES compatibility rendering
- T15: #16 — Implement iOS/iPadOS native media host, Metal and sandbox lifecycle

### Acceptance criteria

- [ ] Golden patterns and real clips match declared image tolerances on each backend.
- [ ] Text, edges and black levels do not regress; unsupported filters explain fallback.
- [ ] GPU cost and working set are reported at 720p/1080p/4K output targets.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Disable a failing filter independently; preserve native frame display and prior user preference.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T18: Port video-derived frame generation with cuts, confidence and backend parity

**GitHub:** [Issue #19](https://github.com/Gourav1100/openps-rs/issues/19)

Parent specification: #1

**Task:** T18 · **Priority:** P1 · **Phase:** 4 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Remote video lacks game-engine depth and motion vectors; FG must use the existing video-motion approach and disclose latency/artifact tradeoffs.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/src/camera.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/camera.rs#L1)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [crates/openps-metal/tests/real_clip.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/real_clip.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Specify and port luma pyramid/motion search/warp/camera compensation from current implementation.
- [ ] Add flow consistency, disocclusion confidence and conservative HUD/border handling.
- [ ] Implement separate interpolation and extrapolation modes, initially qualified at 2x.
- [ ] Add fast hard-cut detection and invalidate history on cut, gap, stall, format or epoch changes.
- [ ] Qualify compute-capable GL/GLES separately; report unavailable on basic devices rather than emulating bad FG.

### Dependencies

- T04: #5 — Extract portable render timing, camera and adaptive-quality policy
- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets
- T07: #8 — Add explicit OpenGL and OpenGL ES compatibility rendering
- T17: #18 — Port spatial upscaling and color pipeline across GPU backends
- T19: #20 — Implement display-driven pacing, thermal budgets and end-to-end latency telemetry

### Acceptance criteria

- [ ] No generated frame crosses a reset/cut or feeds back into decode/semantic inference.
- [ ] Quality report includes held-out real clips, fast pans, UI, particles, flashes and failed flow.
- [ ] Report actual generated/real/presented FPS and latency; never call this standard FSR 3 or claim reduced game input latency.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Runtime disable or repeat latest real frame when confidence, latency or GPU budget fails.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T19: Implement display-driven pacing, thermal budgets and end-to-end latency telemetry

**GitHub:** [Issue #20](https://github.com/Gourav1100/openps-rs/issues/20)

Parent specification: #1

**Task:** T19 · **Priority:** P1 · **Phase:** 4 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Desktop spin/sleep and static quality policy need adaptation for multiple refresh rates, mobile power and observable latency.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-metal/src/timeline.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/timeline.rs#L1)
- [crates/openps-metal/src/ladder.rs:22](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/ladder.rs#L22)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add native presentation clocks/signals and actual display-change handling.
- [ ] Track arrival/decode/queue/GPU/scheduled/observed-present timestamps with validity flags.
- [ ] Support cadence-aware 30/60 content on 59.94/60/90/120/144 Hz and VRR without pretending every refresh is new content.
- [ ] Add user-visible quality versus power policy and thermal hysteresis that reduces FG/upscale/inference work before unstable frame queues.
- [ ] Separate network, intentional interpolation and presentation delays; retain old requested settings during temporary fallback.

### Dependencies

- T01: #2 — Establish replay fixtures, performance baselines and hardware qualification matrix
- T04: #5 — Extract portable render timing, camera and adaptive-quality policy
- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets

### Acceptance criteria

- [ ] Synthetic timing traces never go backward or grow playout unbounded.
- [ ] Reference targets and 30-minute mobile thermal soak pass or produce documented lower capability tiers.
- [ ] Metrics never label CPU submit time as GPU completion or estimated present as observed present.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Use conservative vsync/native resolution policy when timing/thermal APIs are unavailable.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T20: Make scene inference portable and model artifacts reproducible

**GitHub:** [Issue #21](https://github.com/Gourav1100/openps-rs/issues/21)

Parent specification: #1

**Task:** T20 · **Priority:** P1 · **Phase:** 4 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

CoreML/CVPixelBuffer coupling and absent model assets prevent cross-platform scene detection.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-scene/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/lib.rs#L1)
- [crates/openps-scene/src/encoder.rs:61](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/encoder.rs#L61)
- [crates/openps-scene/src/service.rs:126](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/service.rs#L126)
- [tools/scene-context/scenelab/export.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/export.py#L1)
- [tools/scene-context/scenelab/pictures.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/pictures.py#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add ImageEncoder abstraction and portable sampled-frame/tensor input while retaining CoreML adapter.
- [ ] Export ONNX equivalents and validate CPU baseline before selecting accelerated providers.
- [ ] Create content-hashed manifest for weights, tensor schemas, preprocessing, layout, licenses and probe compatibility.
- [ ] Replace mtime/size-only cache identity and implement atomic compile/load/cache cleanup.
- [ ] Bundle or reproducibly fetch artifacts with explicit optional download UI; keep gameplay usable without models.

### Dependencies

- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T12: #13 — Build Android native shell, MediaCodec surfaces and lifecycle integration
- T15: #16 — Implement iOS/iPadOS native media host, Metal and sandbox lifecycle

### Acceptance criteria

- [ ] Identical input fixtures yield matching embeddings/probabilities within documented tolerances across runtimes.
- [ ] Clean installation either has valid model assets or clearly reports unavailable; no silent network fetch.
- [ ] Unsupported execution provider falls back without blocking decode/input.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Retain current Apple encoder and manual mapping while portable runtime/models are unqualified.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T21: Prevent stale scene publication and add inference recovery/freshness state

**GitHub:** [Issue #22](https://github.com/Gourav1100/openps-rs/issues/22)

Parent specification: #1

**Task:** T21 · **Priority:** P0 · **Phase:** 1 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Current running() recheck cannot distinguish a picture from an old session/profile after a rapid off/on or reconnect; repeated inference failures can leave stale context visible.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-scene/src/service.rs:126](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/service.rs#L126)
- [crates/openps-input/src/scene.rs:199](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/scene.rs#L199)
- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [crates/openps-scene/tests/service.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/tests/service.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add session/profile/model generations to samples and results; reject mismatches immediately before publication.
- [ ] Clear pending sample, action queue and filter state on generation changes.
- [ ] Add sample-age limit, failure counters, bounded retry/backoff and explicit degraded/unavailable state.
- [ ] Publish coherent scene/confidence/age/generation snapshot without blocking the input loop.
- [ ] Make model-load failures retryable and rate-limit error logs.

### Dependencies

- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps

### Acceptance criteria

- [ ] A deliberately delayed inference cannot publish after off/on, profile change, reconnect or shutdown.
- [ ] Repeated errors expire the old scene and select safe/manual fallback.
- [ ] One pending sample remains the bound and model load never blocks app exit indefinitely.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Disable automatic mapping on service failure while preserving user-selected/manual layer.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T22: Harden scene probe validation and calibrate mapping decisions

**GitHub:** [Issue #23](https://github.com/Gourav1100/openps-rs/issues/23)

Parent specification: #1

**Task:** T22 · **Priority:** P1 · **Phase:** 4 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Probe validation and argmax-based scene selection need explicit invalid-input and uncertainty policy before mobile model variants are introduced.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-scene/src/probe.rs:82](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/probe.rs#L82)
- [crates/openps-scene/src/filter.rs:72](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/filter.rs#L72)
- [crates/openps-input/src/scene.rs:199](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/scene.rs#L199)
- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Reject malformed class sets, nonfinite coefficients/embeddings, bad dimensions and invalid temperature/scales.
- [ ] Precompute standardized coefficients and reusable inference buffers after correctness tests.
- [ ] Calibrate probabilities on held-out data and add margin/entropy abstention with bounded hold/fallback.
- [ ] Version per-game filter parameters while retaining current 12-second exit-lock behavior as compatibility profile.
- [ ] Make cutscene-to-menu mapping and manual override explicit; release old bindings safely on layer transitions.

### Dependencies

- T21: #22 — Prevent stale scene publication and add inference recovery/freshness state
- T24: #25 — Build game-disjoint scene evaluation with real action traces

### Acceptance criteria

- [ ] Malformed models/outputs never panic or publish invalid distributions.
- [ ] Held-key layer switches cannot create stuck/duplicate controller actions.
- [ ] Threshold selection uses validation data and final held-out switch/error metrics improve or preserve baseline.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Retain compatibility filter profile and manual layer override; do not ship new thresholds without evaluation evidence.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T23: Add controller backends, touch fallback and input-source arbitration

**GitHub:** [Issue #24](https://github.com/Gourav1100/openps-rs/issues/24)

Parent specification: #1

**Task:** T23 · **Priority:** P0 · **Phase:** 3 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

TV/mobile usability depends on controllers; raw DualSense capabilities cannot be assumed through all platform APIs.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-gamepad/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/lib.rs#L1)
- [crates/openps-gamepad/src/hid.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/hid.rs#L1)
- [crates/openps-gamepad/src/map.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/map.rs#L1)
- [crates/openps-gamepad/src/mode.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/mode.rs#L1)
- [src-tauri/src/pads.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/pads.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Implement Windows/Linux and native Android/GameController Apple adapters using existing PadSource/PadSink contracts.
- [ ] Support standard buttons/sticks first and enumerate gyro/touchpad/battery/rumble/trigger capabilities separately.
- [ ] Add minimal touch controls/release menu for phone use without hardware.
- [ ] Define single-owner arbitration among pad, keyboard/mouse and touch; neutralize previous source on handoff.
- [ ] Handle hotplug, reconnect, multiple controllers and identity changes without stale state.

### Dependencies

- T11: #12 — Extract portable input host and implement Windows/Linux capture adapters

### Acceptance criteria

- [ ] Basic console control works on each qualified platform with documented supported devices.
- [ ] Unsupported advanced haptics are reported rather than silently promised.
- [ ] Hotplug/source handoff tests show no held buttons, lost release or duplicate input.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep basic controller functionality when device-specific extras fail; user can choose source manually.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T24: Build game-disjoint scene evaluation with real action traces

**GitHub:** [Issue #25](https://github.com/Gourav1100/openps-rs/issues/25)

Parent specification: #1

**Task:** T24 · **Priority:** P1 · **Phase:** 0 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Current recording holdouts and simulated actions do not fully establish generalization or real mapping-transition safety.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [tools/scene-context/scenelab/evaluate.py:41](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/evaluate.py#L41)
- [tools/scene-context/scenelab/filtersim.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/filtersim.py#L1)
- [tools/scene-context/scenelab/data.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/data.py#L1)
- [tools/scene-context/dataset/recordings.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/dataset/recordings.json#L1)
- [crates/openps-scene/tests/replay.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/tests/replay.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add game/session/source-clip metadata, split hashes and leakage checks.
- [ ] Record real mapped action times and annotated transitions with user-controlled local capture.
- [ ] Keep a final untouched test set and compare baseline, calibration and per-game filter variants.
- [ ] Report macro-F1, class recall, ECE, false switches/minute, transition p95, staleness and uncertainty coverage.
- [ ] Export deterministic Rust replay fixtures for involuntary transitions, repeated failed presses and network/lifecycle gaps.

### Dependencies

- T01: #2 — Establish replay fixtures, performance baselines and hardware qualification matrix

### Acceptance criteria

- [ ] No adjacent/alternate-encode source material crosses train/test splits.
- [ ] Report distinguishes simulated and recorded actions and identifies unknown games.
- [ ] Rust/Python reference parity and baseline replay remain passing.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Evaluation changes are additive; do not replace existing baselines or train on the final test set.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T25: Develop a mobile scene-model tier and adaptive sampling policy

**GitHub:** [Issue #26](https://github.com/Gourav1100/openps-rs/issues/26)

Parent specification: #1

**Task:** T25 · **Priority:** P2 · **Phase:** 4 · **Estimate:** R (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Two encoders totaling 2048 features may be unnecessarily expensive on low-power devices; current 2 Hz schedule is fixed.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [tools/scene-context/scenelab/pictures.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/pictures.py#L1)
- [tools/scene-context/scenelab/features.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/features.py#L1)
- [tools/scene-context/scenelab/export.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/export.py#L1)
- [crates/openps-scene/src/service.rs:126](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/service.rs#L126)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Ablate full model, MobileCLIP-only, distilled student, lower resolution and FP16/INT8 variants.
- [ ] Retrain probes for every changed representation and verify tensor/preprocessing parity.
- [ ] Benchmark cold load, p95 inference, memory and sustained thermal effect alongside decode/FG.
- [ ] Use bounded action-triggered sample bursts and uncertainty-triggered larger inference, with thermal slowdown.
- [ ] Preserve prior crop experiments as evidence; require a controlled new gain before adding HUD crops.

### Dependencies

- T20: #21 — Make scene inference portable and model artifacts reproducible
- T24: #25 — Build game-disjoint scene evaluation with real action traces
- T22: #23 — Harden scene probe validation and calibrate mapping decisions
- T19: #20 — Implement display-driven pacing, thermal budgets and end-to-end latency telemetry

### Acceptance criteria

- [ ] Chosen tier meets held-out false-switch/accuracy gates and memory budget on target phones/TV.
- [ ] No zero-filled substitute embedding is accepted as the original model.
- [ ] Optional ML cannot destabilize decode/audio/input and degrades to manual mapping.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Select original model on capable hardware or disable automatic inference when compact models fail quality gates.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T26: Optimize decode scheduling, texture copies and duplicate detection from measured traces

**GitHub:** [Issue #27](https://github.com/Gourav1100/openps-rs/issues/27)

Parent specification: #1

**Task:** T26 · **Priority:** P1 · **Phase:** 4 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Per-decode VT asynchronous waits, BGRA conversion and CPU IOSurface sampling are concrete optimization candidates; the renderer test-helper wait must not be mistaken for a hot-path stall.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-video/src/media.rs:16](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/media.rs#L16)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Remove per-frame whole-session decode waits through bounded asynchronous completion ownership.
- [ ] Compare NV12 GPU conversion with BGRA and compare zero-copy with application-owned ring copy.
- [ ] Reuse fixed duplicate-signature storage; evaluate GPU downsample/async tiny readback if CPU lock cost matters.
- [ ] Bound texture ring memory by device budget and retain completion-safe leases.
- [ ] Separate each optimization PR and keep counters for copy/lock/wait costs.

### Dependencies

- T01: #2 — Establish replay fixtures, performance baselines and hardware qualification matrix
- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps
- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets

### Acceptance criteria

- [ ] Before/after replay on identical hardware demonstrates lower stage latency or bandwidth without increasing frame age.
- [ ] No decoder surface is reused early and reset/callback tests pass.
- [ ] Do not remove test-helper or shutdown waits solely because their names appear in a search.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Feature-flag asynchronous/import changes and retain proven one-copy path for problematic drivers.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T27: Optimize transport queues, assembly allocations and FEC without changing protocol behavior

**GitHub:** [Issue #28](https://github.com/Gourav1100/openps-rs/issues/28)

Parent specification: #1

**Task:** T27 · **Priority:** P2 · **Phase:** 4 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Queue counts alone do not bound frame age, packet assembly copies each unit, and FEC work may be material under loss.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-session/src/runner.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-session/src/runner.rs#L1)
- [crates/openps-session/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-session/src/lib.rs#L1)
- [crates/openps-stream/src/video.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-stream/src/video.rs#L1)
- [crates/openps-stream/src/stream.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-stream/src/stream.rs#L1)
- [crates/openps-fec/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-fec/src/lib.rs#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Instrument queue bytes/age, control latency and allocations before changing data structures.
- [ ] Prioritize lifecycle/neutral input; coalesce only safe latest-value state while preserving button transitions and protocol history.
- [ ] Audit inbound/notice counters on LinkClosed/failed sends and every dequeue; add invariant tests.
- [ ] Pool packet/assembly scratch storage within existing resource limits.
- [ ] Benchmark bounded matrix caching and CPU-feature-dispatched SIMD FEC against scalar oracle.

### Dependencies

- T01: #2 — Establish replay fixtures, performance baselines and hardware qualification matrix
- T03: #4 — Define owned media frames, decoder interfaces and epoch-aware timestamps

### Acceptance criteria

- [ ] Replay/fuzz/resource tests are bit-equivalent and memory stays bounded under malicious/overloaded input.
- [ ] Stop/release remains prompt under datagram flood.
- [ ] Measured speedup is reported for realistic loss patterns; no performance-only test replaces correctness checks.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep scalar FEC/reference assembly and disable optimizations independently when device-specific benefit is absent.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T28: Implement native storage, credential protection and migration-safe app paths

**GitHub:** [Issue #29](https://github.com/Gourav1100/openps-rs/issues/29)

Parent specification: #1

**Task:** T28 · **Priority:** P0 · **Phase:** 1 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Executable-relative writes and Unix-only permissions are unsuitable for Windows installation and signed mobile bundles.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [src-tauri/src/paths.rs:25](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/paths.rs#L25)
- [crates/openps-core/src/store.rs:255](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-core/src/store.rs#L255)
- [crates/openps-psn/src/store.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-psn/src/store.rs#L1)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [scripts/install.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/install.sh#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Use platform application-data/cache directories by default and explicit desktop portable mode.
- [ ] Add Windows private-file ACL/atomic replacement and platform-native credential stores; keep preferences separately versioned.
- [ ] Migrate existing portable files by copying and verifying before switching location, with rollback metadata.
- [ ] Ensure sign-out/removal clears correct credentials and no diagnostic exports contain secrets.
- [ ] Update model cache, logs, keymaps and backup locations consistently across shell/install scripts.

### Dependencies

- T02: #3 — Remove platform compile blockers and introduce target-specific dependency boundaries

### Acceptance criteria

- [ ] Read-only install, interrupted write/migration, concurrent save and unavailable credential-store tests pass.
- [ ] Permissions and atomic replacement are verified on Windows and Unix hosts.
- [ ] No mobile path writes into application bundle; old profiles/pairings remain recoverable.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Never delete original data until verified migration succeeds; support a documented return to prior storage path.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T29: Add capability-driven settings, mobile/TV layouts and truthful diagnostics

**GitHub:** [Issue #30](https://github.com/Gourav1100/openps-rs/issues/30)

Parent specification: #1

**Task:** T29 · **Priority:** P1 · **Phase:** 3 · **Estimate:** M (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Current UI is MetalFX/FSR-specific and uses legacy settings encodings that do not represent separate backend, decoder and algorithm choices.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-core/src/types.rs:167](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-core/src/types.rs#L167)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)
- [dist/index.html:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/index.html#L1)
- [dist/style.css:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/style.css#L1)
- [tests/display-settings.test.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tests/display-settings.test.js#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Version settings and migrate existing engine/interpolator/frame_mode/frame_gen semantics.
- [ ] Add requested/effective backend, decoder, upscaler, FG and inference tier plus failure reasons.
- [ ] Expose copy route, unique/real/generated/presented FPS, queue age and timing validity.
- [ ] Implement responsive landscape phone/tablet and remote-first TV layouts with accessible release and fallback messaging.
- [ ] Replace misleading zero-latency/unsupported API wording while retaining legacy setting compatibility.

### Dependencies

- T04: #5 — Extract portable render timing, camera and adaptive-quality policy
- T06: #7 — Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets
- T11: #12 — Extract portable input host and implement Windows/Linux capture adapters
- T28: #29 — Implement native storage, credential protection and migration-safe app paths

### Acceptance criteria

- [ ] Old settings round-trip with equivalent intent and unknown future values fail safely.
- [ ] Disabled options explain capability/runtime reasons; no false active state.
- [ ] Frontend tests cover platform capability matrices, migration and keyboard/D-pad navigation.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep prior requested preferences even when temporarily unavailable; migration can restore previous schema backup.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T30: Add cross-platform CI, GPU/media conformance and hardware release qualification

**GitHub:** [Issue #31](https://github.com/Gourav1100/openps-rs/issues/31)

Parent specification: #1

**Task:** T30 · **Priority:** P0 · **Phase:** 1 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

The snapshot has no tracked GitHub workflow directory; mocks and source assertions cannot qualify platform graphics/input behavior.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/Cargo.toml#L1)
- [scripts/build.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/build.sh#L1)
- [crates/openps-video/tests/pipeline.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/tests/pipeline.rs#L1)
- [crates/openps-metal/tests/replay.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/replay.rs#L1)
- [crates/openps-scene/tests/reference.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/tests/reference.rs#L1)
- [tests/display-settings.test.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tests/display-settings.test.js#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add proposed .github/workflows for fmt/clippy/portable tests, desktop native builds, Android ABIs and iOS compile checks.
- [ ] Pin Rust/SDK/NDK/Xcode and native dependencies; keep secrets out of PR jobs.
- [ ] Add decoder/color/FG/lifecycle golden suites with explicit hardware runner capabilities.
- [ ] Execute PS4/PS5 LAN/WAN matrix, input/orientation tests, mobile thermal and desktop soak plans as implementations land.
- [ ] Store machine-readable environment/results and distinguish compile/mock/emulator/real hardware evidence.

### Dependencies

- T01: #2 — Establish replay fixtures, performance baselines and hardware qualification matrix
- T02: #3 — Remove platform compile blockers and introduce target-specific dependency boundaries

### Acceptance criteria

- [ ] Ordinary PR CI is deterministic with redistributable fixtures.
- [ ] Release checklist requires actual platform playback/audio/input and tested fallback paths.
- [ ] No hardware-unavailable job is silently treated as successful qualification.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Keep optional hardware suites separately scheduled; failed optional features can be excluded from declared support but core platform failures block release.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T31: Package, sign and document platform releases and model/license provenance

**GitHub:** [Issue #32](https://github.com/Gourav1100/openps-rs/issues/32)

Parent specification: #1

**Task:** T31 · **Priority:** P1 · **Phase:** 5 · **Estimate:** L (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

Distribution is currently macOS-centric and README has contradictory constants/license claims plus links outside the repository.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [README.md:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/README.md#L1)
- [RELEASE_NOTES.md:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/RELEASE_NOTES.md#L1)
- [docs/USAGE.md:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/docs/USAGE.md#L1)
- [src-tauri/tauri.conf.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/tauri.conf.json#L1)
- [scripts/build.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/build.sh#L1)
- [scripts/install.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/install.sh#L1)
- [scripts/make-dmg.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/make-dmg.sh#L1)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Add Windows installer, Linux chosen packages, Android app/TV artifacts and iOS archive/signing instructions.
- [ ] Inventory code/model/codec/shader licenses and notices; resolve project license with maintainers before publishing binaries.
- [ ] Document reproducible model acquisition, cache paths, platform capability/device matrix and unsupported combinations.
- [ ] Replace sibling analysis links with in-repo architecture/test docs and reconcile README claims with actual tracked content.
- [ ] Validate fresh install/update/uninstall, offline startup, crash diagnostics redaction and signing/release provenance.

### Dependencies

- T08: #9 — Implement Windows Media Foundation decode and native gameplay host
- T09: #10 — Implement Linux decode, Wayland/X11 surfaces and distribution integration
- T12: #13 — Build Android native shell, MediaCodec surfaces and lifecycle integration
- T14: #15 — Deliver Android TV launcher, remote-first UX and low-resource qualification
- T15: #16 — Implement iOS/iPadOS native media host, Metal and sandbox lifecycle
- T16: #17 — Implement iOS/iPadOS keyboard, relative mouse capture and landscape controller
- T28: #29 — Implement native storage, credential protection and migration-safe app paths
- T29: #30 — Add capability-driven settings, mobile/TV layouts and truthful diagnostics
- T30: #31 — Add cross-platform CI, GPU/media conformance and hardware release qualification

### Acceptance criteria

- [ ] Every advertised artifact installs and passes core real-device smoke tests.
- [ ] No release claims an untested decoder/backend/lock mode; no private credential files enter packages.
- [ ] Maintainer licensing decisions and model provenance are recorded before distribution.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Use prerelease channels and preserve previous installers/settings rollback; this task does not authorize publishing unqualified builds.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.


---

## T32: Evaluate optional Vulkan-over-Metal and advanced temporal reconstruction feasibility

**GitHub:** [Issue #33](https://github.com/Gourav1100/openps-rs/issues/33)

Parent specification: #1

**Task:** T32 · **Priority:** P2 · **Phase:** 5 · **Estimate:** R (M: 1–2 weeks; L: 2–4 weeks; R: one-week feasibility time box). Estimates are preliminary.

### Problem

A request for Vulkan and advanced upscaling should be assessed honestly on iOS and video-only inputs rather than promised as native API parity.

### Implementation locations

Existing sources at audited commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`:

- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)

New modules named below are proposed; follow the parent spec's target layout.

### Implementation checklist

- [ ] Time-box Vulkan Portability/MoltenVK experiment on Apple targets only after native Metal works.
- [ ] Measure binary size, native texture import, latency and shader parity against production Metal.
- [ ] Evaluate any video-only temporal super-resolution proposal as a separate algorithm with held-out artifact/latency tests.
- [ ] Document why game-engine FSR/DLSS/XeSS paths requiring unavailable buffers are not plug-compatible.
- [ ] Publish ADR deciding ship experimentally, defer or reject with evidence; keep DirectX Windows-only.

### Dependencies

- T15: #16 — Implement iOS/iPadOS native media host, Metal and sandbox lifecycle
- T17: #18 — Port spatial upscaling and color pipeline across GPU backends
- T18: #19 — Port video-derived frame generation with cuts, confidence and backend parity
- T30: #31 — Add cross-platform CI, GPU/media conformance and hardware release qualification

### Acceptance criteria

- [ ] Research produces reproducible evidence and explicit compatibility table even if decision is not to ship.
- [ ] No fake depth/motion data is passed to an SDK and described as correct integration.
- [ ] Native iOS playback and portable spatial upscale remain independent of this experiment.
- [ ] Attach targeted test/replay results with environment details; distinguish simulated and physical-device evidence.
- [ ] Update user-facing capabilities/documentation and preserve macOS regression coverage where relevant.

### Rollback / fallback

Leave native Metal/spatial upscale as production defaults; no experimental dependency required for core builds.

### Scope control

Implement this work package against current main after verifying the pinned audit references. Do not mark a platform or feature supported solely from compilation. Full contracts, failure behavior, quality/performance targets and release gates are in #1.

