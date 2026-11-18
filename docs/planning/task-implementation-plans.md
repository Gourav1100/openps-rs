# OpenPS per-task implementation plans

Prepared 17 November 2026. These plans supplement the [master specification](https://github.com/Gourav1100/openps-rs/issues/1) and the existing issue acceptance criteria. All native interfaces and new paths below are proposals; current implementation remains the audited macOS-centric baseline.

## Delivery sequence and ownership

1. Start T01 baseline/replay, T05 interop and T24 data evaluation. Run mobile input feasibility slices of T13/T16 early using isolated native harnesses; their production integration still waits for the host/input contracts.
2. T02 establishes compile boundaries; T03 media identity/leases and T04 portable rendering policy define shared contracts. T28 storage and initial T30 CI land alongside them. T21 can land a localized freshness fix early, then adopt shared epochs.
3. T06/T07 renderer backends, T10 audio and T11 input make T08/T09 playable desktop integration possible. T12/T15 mobile hosts then integrate those contracts; T13/T16 complete native capture/orientation, T23 supplies controller/touch support, and T14 delivers TV flows.
4. T17 spatial scaling precedes T18 frame generation. T19 pacing/telemetry supports both; T20 portable inference, T22 calibrated decisions and T25 mobile tiers build on T21/T24. T26/T27 optimize measured bottlenecks rather than redefining their shared interfaces.
5. T29 capability/settings/UI migration is an integration dependency for the finished phone/TV experience. T30 runs continuously; T31 packages only qualified features. T32 is optional research and never blocks native iOS support.

**Avoid overlapping implementations:** T03 owns decoder contracts/lifetimes; T26 optimizes their Apple implementation. T04 owns pure policy extraction; T19 owns new pacing/budget behavior. T11 owns native event normalization/capture semantics; T23 owns input-source arbitration. T20 owns model/runtime artifacts; T21 owns service freshness; T22 owns decision policy; T25 owns model-tier/sampling experiments. T02 adds minimal portable storage seams; T28 owns full native storage and migration.

**Estimates:** retain issue estimates until feasibility reports exist. The numbered PR slices below are implementation ordering, not calendar commitments. Performance targets remain proposed gates, not measurements.

**Review refinements:** the actual decoder accessor is `Frame::source_id()`; the UI already labels `zero_latency` as Low latency. `openps-core/src/netlink.rs` also needs platform gating for its macOS subprocess probes. The session LinkClosed counter path needs a focused regression test before broader queue optimization. T14 requires T29 UI integration before final acceptance.

## Task index

- [T01: Establish replay fixtures, performance baselines and hardware qualification matrix](https://github.com/Gourav1100/openps-rs/issues/2)
- [T02: Remove platform compile blockers and introduce target-specific dependency boundaries](https://github.com/Gourav1100/openps-rs/issues/3)
- [T03: Define owned media frames, decoder interfaces and epoch-aware timestamps](https://github.com/Gourav1100/openps-rs/issues/4)
- [T04: Extract portable render timing, camera and adaptive-quality policy](https://github.com/Gourav1100/openps-rs/issues/5)
- [T05: Prototype decoder-to-GPU interoperability before committing backend design](https://github.com/Gourav1100/openps-rs/issues/6)
- [T06: Implement shared Vulkan and D3D12 renderer with capabilities and resource budgets](https://github.com/Gourav1100/openps-rs/issues/7)
- [T07: Add explicit OpenGL and OpenGL ES compatibility rendering](https://github.com/Gourav1100/openps-rs/issues/8)
- [T08: Implement Windows Media Foundation decode and native gameplay host](https://github.com/Gourav1100/openps-rs/issues/9)
- [T09: Implement Linux decode, Wayland/X11 surfaces and distribution integration](https://github.com/Gourav1100/openps-rs/issues/10)
- [T10: Port Opus decoding and audio output with independent A/V clock policy](https://github.com/Gourav1100/openps-rs/issues/11)
- [T11: Extract portable input host and implement Windows/Linux capture adapters](https://github.com/Gourav1100/openps-rs/issues/12)
- [T12: Build Android native shell, MediaCodec surfaces and lifecycle integration](https://github.com/Gourav1100/openps-rs/issues/13)
- [T13: Implement Android pointer lock, physical keyboard mapping and fixed-landscape gameplay](https://github.com/Gourav1100/openps-rs/issues/14)
- [T14: Deliver Android TV launcher, remote-first UX and low-resource qualification](https://github.com/Gourav1100/openps-rs/issues/15)
- [T15: Implement iOS/iPadOS native media host, Metal and sandbox lifecycle](https://github.com/Gourav1100/openps-rs/issues/16)
- [T16: Implement iOS/iPadOS keyboard, relative mouse capture and landscape controller](https://github.com/Gourav1100/openps-rs/issues/17)
- [T17: Port spatial upscaling and color pipeline across GPU backends](https://github.com/Gourav1100/openps-rs/issues/18)
- [T18: Port video-derived frame generation with cuts, confidence and backend parity](https://github.com/Gourav1100/openps-rs/issues/19)
- [T19: Implement display-driven pacing, thermal budgets and end-to-end latency telemetry](https://github.com/Gourav1100/openps-rs/issues/20)
- [T20: Make scene inference portable and model artifacts reproducible](https://github.com/Gourav1100/openps-rs/issues/21)
- [T21: Prevent stale scene publication and add inference recovery/freshness state](https://github.com/Gourav1100/openps-rs/issues/22)
- [T22: Harden scene probe validation and calibrate mapping decisions](https://github.com/Gourav1100/openps-rs/issues/23)
- [T23: Add controller backends, touch fallback and input-source arbitration](https://github.com/Gourav1100/openps-rs/issues/24)
- [T24: Build game-disjoint scene evaluation with real action traces](https://github.com/Gourav1100/openps-rs/issues/25)
- [T25: Develop a mobile scene-model tier and adaptive sampling policy](https://github.com/Gourav1100/openps-rs/issues/26)
- [T26: Optimize decode scheduling, texture copies and duplicate detection from measured traces](https://github.com/Gourav1100/openps-rs/issues/27)
- [T27: Optimize transport queues, assembly allocations and FEC without changing protocol behavior](https://github.com/Gourav1100/openps-rs/issues/28)
- [T28: Implement native storage, credential protection and migration-safe app paths](https://github.com/Gourav1100/openps-rs/issues/29)
- [T29: Add capability-driven settings, mobile/TV layouts and truthful diagnostics](https://github.com/Gourav1100/openps-rs/issues/30)
- [T30: Add cross-platform CI, GPU/media conformance and hardware release qualification](https://github.com/Gourav1100/openps-rs/issues/31)
- [T31: Package, sign and document platform releases and model/license provenance](https://github.com/Gourav1100/openps-rs/issues/32)
- [T32: Evaluate optional Vulkan-over-Metal and advanced temporal reconstruction feasibility](https://github.com/Gourav1100/openps-rs/issues/33)

<!-- openps-implementation-plan-v1:T01 -->
## T01 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** None; this task can start from the audited baseline.

### Existing code entry points

- [crates/openps-testkit/src/stream.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-testkit/src/stream.rs#L1)
- [crates/openps-demo/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-demo/src/lib.rs#L1)
- [crates/openps-metal/tests/stage_cost.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/stage_cost.rs#L1)
- [crates/openps-stream/tests/perf.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-stream/tests/perf.rs#L1)

### File-level implementation

- **Extend `crates/openps-testkit/src/stream.rs`** with a replay source that yields recorded datagrams and a deterministic monotonic offset. Keep source-time and delivery-time separate so loss/jitter injection does not rewrite capture timing.
- **Add `crates/openps-testkit/src/replay.rs` and `tests/replay.rs` (proposed)** for `ReplayManifest`, `ReplayEvent`, seeded loss/reorder/duplicate transforms, and a fake clock. A manifest records SHA-256, codec/profile, resolution, frame cadence, source rights, generator command and expected discontinuities.
- **Extend `crates/openps-demo/src/lib.rs`** to select these fixtures through configuration while retaining its existing pretend-console API. Keep replay usable without a model download or live Sony access.
- **Add `tools/perf/run_replay.py`, `tools/perf/report.py`, `docs/testing/device-matrix.md` and `docs/testing/baseline.md` (proposed)**. Reports capture commit, build profile, device/driver/OS, source and display rates, enabled effects, model hash, stage p50/p95/p99, drops and memory.

### Execution order / PR slices

1. Land fixture schema, hashes and deterministic transport replay first; two runs with one seed must produce identical event ordering.
2. Wire existing `openps-stream/tests/perf.rs`, `openps-metal/tests/stage_cost.rs`, `periodic.rs` and `real_clip.rs` into a report format without rewriting their algorithms.
3. Record current macOS baseline with native/off, spatial upscale and both FG modes. Register unavailable platforms as **not measured**; publish the qualification matrix for later adapter tasks.

### Decisions and invariants

Use a versioned JSON manifest plus small binary media/datagram files; do not store captures with account tokens or registration keys. Prefer generated footage for CI, with a separate opt-in real-game corpus. Measure steady-state separately from warm-up/model compilation. A frame trace needs a correlation ID now; migrate it to T03's identity later. The benchmark harness must not force a dependency on a new renderer before it exists.

### Targeted validation / completion evidence

Run existing stream replay/resource tests and new deterministic seed tests; validate hashes on load and reject oversized fixtures before allocation. Save baseline reports rather than embedding expected timing thresholds into generic CI. Include a physical-device inventory with owners/availability fields left unassigned until confirmed. Completion requires reproducible commands and a baseline artifact, not asserted speedups.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T02 -->
## T02 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T01 / #2](https://github.com/Gourav1100/openps-rs/issues/2)

### Existing code entry points

- [Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/Cargo.toml#L1)
- [src-tauri/Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/Cargo.toml#L1)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [src-tauri/src/paths.rs:25](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/paths.rs#L25)

### File-level implementation

- **`src-tauri/Cargo.toml` / root `Cargo.toml`:** introduce the minimal portable render-contract crate and target-specific dependency tables. Keep `objc2`, Apple frameworks and native implementation crates out of Windows/Linux dependency graphs. T04 completes extraction; this task only establishes the minimum types needed to compile.
- **`src-tauri/src/lib.rs`:** change the signatures of `media_and_events` and `build_core`, and setup's `SharedLook::new()` call, to the portable type. Add a platform factory returning typed availability rather than a seemingly playable `NullMedia` path.
- **`src-tauri/src/paths.rs`, `crates/openps-core/src/store.rs`, `crates/openps-psn/src/store.rs`:** isolate Unix-only imports/mode calls into target modules. Introduce a minimal private-file helper; T28 supplies the complete credential/migration behavior. Do not temporarily replace private writes with world-readable files just to obtain a green Windows build.
- **`crates/openps-core/src/netlink.rs`:** `probe()` currently launches macOS tools such as `networksetup` and `system_profiler`. Keep pure parsing/bitrate policy portable, gate the native probe, and return Unknown/default conservatively elsewhere until a native link-information adapter exists.
- **Native `build.rs` files and `openps-metal/Cargo.toml`:** audit linkage independently from Rust `cfg`; target selection must prevent loading desktop frameworks on mobile.

### Execution order / PR slices

1. Add target compile inventory and platform module skeletons; make portable libraries compile with no shell.
2. Fix unguarded type/path references and private-file helpers; compile the shell with an explicit unavailable-media adapter.
3. Audit `demo` and `psn-live` feature combinations: `openps-demo` is currently target-scoped yet referenced by feature-gated shell code. Separate demo media production from platform decode dependencies as needed.

### Targeted validation / completion evidence

Run `cargo check` for portable crate groups on native Windows/Linux runners, then shell/default/demo/psn-live combinations where supported. Run targeted storage tests on both OS families and macOS regression tests. Inspect Cargo dependency trees to prove no Apple library leaks into non-Apple builds. Record remaining runtime gaps explicitly. A successful stub build closes only this compile-boundary task, not platform-support tasks.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T03 -->
## T03 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T02 / #3](https://github.com/Gourav1100/openps-rs/issues/3)

### Existing code entry points

- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)
- [crates/openps-video/src/media.rs:16](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/media.rs#L16)
- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-core/src/events.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-core/src/events.rs#L1)

### File-level implementation

- **Add `crates/openps-media/src/{frame,time,format,capabilities}.rs` (proposed):** define `SessionEpoch`, `FrameKey { epoch, sequence }`, `MediaTimestamp { ticks, timebase, provenance }`, `FrameFormat`, `ColorInfo` and `DecodedFrame`. Use a native resource lease with explicit callback/release ownership; opaque handles must not become unconditionally Send/Sync.
- **`crates/openps-video/src/pipeline.rs`:** replace concrete `Option<vt::Decoder>` with a decoder interface or factory. Keep `FailureStreak`, parameter-set selection, loss handling and IDR throttle in this portable policy layer. Separate submit errors from asynchronous output errors.
- **Add `crates/openps-video/src/decoder/{mod,apple}.rs` (proposed):** contract sketch: `configure(config)`, `submit(access_unit) -> Accepted|Backpressure`, `poll_output() -> DecodedFrame|DecodeEvent`, `flush(reason, epoch)` and `close()`. These are proposed API shapes, not existing methods.
- **`vt.rs`:** adapt callback context to retain `FrameKey` separately from CMTime; audit buffer ownership until callback completion. The actual existing accessor is `Frame::source_id()` (not `id()`): remove its dependency on dividing PTS by 1500 once all call sites use explicit identity.
- **`media.rs` / `openps-core/src/events.rs`:** retain encoded `MediaSink` at the core boundary; make `PictureSink` consume portable frames. Do not introduce a media→core dependency cycle.

### Execution order / PR slices

1. Land contracts plus fake decoder, wrapping the existing synchronous VT behavior without changing scheduling.
2. Migrate renderer/scene consumers to explicit identity and native leases through compatibility adapters.
3. Add asynchronous submission and bounded output delivery; T26 handles measured tuning rather than a second competing decoder redesign.

### Targeted validation / completion evidence

Add fake-decoder tests for accepted-no-output, delayed error, duplicate callback, late old-epoch output, reconfigure and backpressure. Test 16-bit sequence wrap, reconnect reset and 30/60 fps independently of identity. A drop-counting lease must prove each sample is released once after the final consumer. Rebuild/stop tests deliberately delay callbacks; never free callback context before the native decoder guarantees quiescence. Keep `cargo test -p openps-video` and pipeline/loss tests as the starting gate; hardware tests remain a separate Apple-host check.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T04 -->
## T04 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4)

### Existing code entry points

- [crates/openps-metal/src/timeline.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/timeline.rs#L1)
- [crates/openps-metal/src/ladder.rs:22](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/ladder.rs#L22)
- [crates/openps-metal/src/look.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/look.rs#L1)
- [crates/openps-metal/src/camera.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/camera.rs#L1)

### File-level implementation

- **New `openps-render/src/{timeline,ladder,camera,look}.rs`:** move the corresponding `openps-metal/src` modules mechanically, retaining public names and numerical behavior. Review their imports first; anything native stays behind a host adapter.
- **New `openps-render/src/{settings,stats,display}.rs`:** move platform-neutral `RenderSettings`, frame/upscale mode enums and statistics out of `renderer.rs`. Split `DisplayInfo`/timing observations from global `NSScreen` discovery and layer state.
- **`openps-metal/src/lib.rs`:** re-export moved public APIs for a compatibility period. Keep `MetalRenderer`, device objects, shaders and native presentation implementation here.
- **`src-tauri/src/lib.rs`, `video.rs`, `input.rs`, `pads.rs`:** use portable `SharedLook`/settings types; inject display information through a narrow interface rather than importing Metal to read policy.

### Execution order / PR slices

1. Move `look` and `timeline` with their tests; use compatibility exports so most consumers need no behavior change.
2. Move camera/ladder and their replay fixtures. Extract settings/stats only after native handles are separated.
3. Add explicit `DisplayInfo` updates for display migration and a fake clock/timing source for cross-platform tests. T19 owns new pacing/thermal policy.

### Decisions and invariants

Keep a one-way graph: media contracts → render policy → platform GPU implementation → shell. Render policy must not depend on Tauri, decoder FFI or Objective-C. The existing ladder uses `STEP_HZ = 5`; preserve that value during extraction and correct documentation separately. Preserve stream-reset, duplicate and wrap semantics before adding any new algorithm. Do not split every pure module into its own crate.

### Targeted validation / completion evidence

Move or adapt `tester_timeline`, `periodic`, `camera_motion`, `tester_limits` and replay tests so policy tests can execute on non-Apple hosts. Compare before/after serialized trace outputs for deterministic fixtures. Any changed estimate should be explained in a separate behavioral PR, not hidden in file moves. Verify the old paths still compile for macOS and that `openps-render`'s Cargo tree has no Apple dependencies.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T05 -->
## T05 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T01 / #2](https://github.com/Gourav1100/openps-rs/issues/2)

### Existing code entry points

- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)

### File-level implementation

- **Add `tools/interop/README.md` and isolated native examples (proposed):** `windows-mf-d3d12`, `linux-vaapi-vulkan`, `android-codec-gles`, `android-codec-vulkan`. Keep optional native dependencies out of ordinary workspace builds until selected.
- **Add `docs/architecture/adr-001-native-frame-interop.md`:** each experiment records producer format, device identity, resource share/export flags, acquire/release primitive, import mechanism, maximum retained decoder surfaces and tested fallback.
- Use existing `vt.rs`/`renderer.rs::copy_in` ownership behavior as a comparison, not as evidence that other APIs share identical semantics.

### Execution order / PR slices

1. Render a CPU-generated color/stride test pattern on the chosen native device.
2. Decode a short T01 clip, first through an explicit copy route, then through native resource sharing. Add a lease tracker that logs acquire, submit, fence completion and release.
3. Deliberately exhaust/hold decoder surfaces, resize while work is in flight and destroy the surface. Compare one-copy with zero-copy latency and pool pressure.
4. Pin the GPU library and write the ADR before T06/T08/T09/T12 build on the result.

### Platform-specific questions to answer

Windows: can the selected MFT's D3D11 output be shared directly, or must it be copied into an application-owned shared texture; how is synchronization represented on D3D12/Vulkan? Linux: which DMA-BUF modifiers and planes are exported, and does the renderer use the same physical GPU? Android: does the codec produce a usable ImageReader/AHardwareBuffer configuration, and does the native importer support its external format/YCbCr conversion? A GLES SurfaceTexture path is a distinct fallback, not proof of Vulkan support.

### Targeted validation / completion evidence

Use synthetic color bars, padded rows and visible frame numbers to catch wrong planes/stride/reuse. A 100-cycle recreate test must end with all leases released. Save device-specific results and traces, including failed import attempts. A time-boxed negative result is acceptable if the ADR chooses a safe copy/native fallback. Do not implement undocumented raw-handle casts to force an import through wgpu. Its [backend support](https://docs.rs/wgpu/latest/wgpu/struct.Backends.html) does not itself promise decoder-resource interoperability.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T06 -->
## T06 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T04 / #5](https://github.com/Gourav1100/openps-rs/issues/5), [T05 / #6](https://github.com/Gourav1100/openps-rs/issues/6)

### Existing code entry points

- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)

### File-level implementation

- **New `openps-render-wgpu/src/{device,surface,import,resources,graph,renderer}.rs`:** device selection/capabilities, platform-surface binding, T05-approved imports, budgeted pools, render-pass graph and renderer worker.
- **New `shaders/{yuv,blit}.wgsl`:** initial crop/color conversion and present pass only. T17 adds spatial filters and T18 adds motion generation.
- **`src-tauri/src/platform/{mod,windows,linux,android}.rs` (proposed):** supply native surface lifecycle messages and choose the renderer through the platform factory; keep UI-thread native handles outside render policy.
- **`openps-render/src/capabilities.rs` (proposed):** define backend, import route, supported formats, maximum dimensions, timestamp-query validity and feature availability with reason codes.

### Execution order / PR slices

1. CPU upload → blit → present on Windows D3D12 and Linux Vulkan, including resize/zero-size suspension. Use the same color/crop fixtures on both.
2. Add imported-frame path selected by T05; preserve CPU/copy fallback and expose the active route.
3. Add bounded in-flight records, resource pooling, device-loss reconstruction and runtime capability reporting.

### Proposed ownership/API boundary

`Renderer::submit(frame)` places a lease in a bounded mailbox; render thread chooses a drawable and records commands. `InFlightSubmission { fence_or_index, leases, pooled_resources, frame_key }` owns all resources until completion. A surface epoch invalidates queued work when the host view is replaced. Resize waits only for resources actually being replaced, not for arbitrary global GPU idle. Oversized allocation requests are checked using overflow-safe byte calculations before calling the driver.

### Targeted validation / completion evidence

Add headless pass tests plus native present tests; cover odd width/crop, padded strides, full/limited range and device-loss injection. Inspect validation-layer errors on D3D12/Vulkan and require none attributable to this code. Test unsupported forced backend versus Auto selection. T19 supplies final pacing; this task still needs bounded vsync presentation and honest stats. Complete with a basic video renderer, not all optional effects, and do not reroute macOS production through wgpu as part of this change.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T07 -->
## T07 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T04 / #5](https://github.com/Gourav1100/openps-rs/issues/5), [T05 / #6](https://github.com/Gourav1100/openps-rs/issues/6)

### Existing code entry points

- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [src-tauri/Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/Cargo.toml#L1)

### File-level implementation

- **New `openps-render-gl/src/{context,import,renderer,capabilities}.rs`:** native desktop context and EGL/GLES adapters behind target gates, thread-bound context ownership, import/upload choice and actual feature queries.
- **New `openps-render-gl/shaders/{blit,yuv,external_oes}.glsl`:** separate regular 2D and Android external-texture shader variants; do not bind an external OES image to a sampler2D path.
- **Platform factory:** add GL/GLES selection and report `native_gl`, `gles` or `angle` distinctly. Windows native OpenGL must use an actual native context if requested; wgpu's ANGLE route is not the same driver path.

### Execution order / PR slices

1. Propose and validate baselines: desktop GL 3.3 and GLES 3.0 for basic playback/spatial fragment passes, subject to surface/import constraints. Query limits instead of inferring them from version strings alone.
2. Implement CPU planes/upload and aspect-fit presentation, then native external-texture/import paths from T05. Use PBO/fence rings only after a straightforward path is correct.
3. Share T17's algorithm reference while translating pass variants for the available GLSL profile. T18 can add compute on qualified desktop GL 4.3/GLES 3.1+ tiers separately.

### Decisions and invariants

All context creation/currentness/destruction must follow one documented thread policy. Keep explicit render state setup so WebView/native compositors cannot leak state into a pass. Surface/context replacement increments a resource generation and invalidates old textures. Collect compile/link diagnostics once; cache shader variants by format/feature tier. Basic playback must not depend on image load/store or compute shaders.

### Targeted validation / completion evidence

Test a no-compute device path, EGL context loss, desktop resize/fullscreen, padded YUV planes and Android external-image orientation. Compare spatial/color outputs to the same fixtures as Vulkan/D3D12 within declared tolerance. Report precise missing feature reasons and actual renderer string. A failing optional shader falls back to native/bilinear presentation; do not silently report FG active. Keep the GL driver/runtime decision in the T05 ADR and verify it against the [documented shared-library backend behavior](https://docs.rs/wgpu/latest/wgpu/struct.Backends.html).

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T08 -->
## T08 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T02 / #3](https://github.com/Gourav1100/openps-rs/issues/3), [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T05 / #6](https://github.com/Gourav1100/openps-rs/issues/6), [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7)

### Existing code entry points

- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)
- [crates/openps-video/src/nal.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/nal.rs#L1)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)

### File-level implementation

- **New `openps-video/src/decoder/windows/{mod,mft,device}.rs`:** codec enumeration, asynchronous MFT state machine and D3D11 device manager. Initialize COM on the owning decode thread and keep all callbacks/resources valid through shutdown.
- **`pipeline.rs` / `nal.rs`:** preserve existing stream-header/parameter-set detection; map access units into the selected MFT's required sample representation and carry T03 identity separately from timestamps.
- **New `src-tauri/src/platform/windows.rs`:** gameplay child window/swapchain host, DPI and size notifications, WebView2 stacking/hit-testing and show/hide on AppEvent session changes. Preserve the existing shell command/event contract.
- **Platform capability factory:** expose H.264/HEVC profiles and accepted output formats, then choose compatible decoder-renderer pairs rather than selecting them independently.

### Execution order / PR slices

1. Native MFT decode to a validated copy/upload path with H.264; explicitly handle need-more-input, output events and stream-format change.
2. Add HEVC enumeration, negotiated fallback and T05 D3D11→D3D12 interop. Check same-adapter identity and retain samples until consumer completion.
3. Integrate native host and full session lifecycle with T10 audio/T11 input; then qualify Windows Vulkan as an alternate renderer using its own import/copy path.

### Decisions and invariants

Represent decoder states `Unconfigured → Configured → Streaming → Flushing/Draining → Closed` explicitly. Prevent callbacks from a removed device or old epoch from enqueueing output. On stream change, invalidate old output format before allocating new surfaces. A missing HEVC implementation must produce H.264 renegotiation if supported by the session, or a clear error—not a fabricated successful stream. Avoid opening the UI on the decode worker.

### Targeted validation / completion evidence

Add MFT-state tests behind an abstraction and native integration tests for first frame, size change, packet loss/IDR, unavailable codec and device removal. Repeat connect/disconnect and monitor/DPI changes. Capture decode/import/present timings on Intel/AMD/NVIDIA reference hardware where available, and document untested combinations. Use [Microsoft's D3D11 decoder integration](https://learn.microsoft.com/en-us/windows/win32/medfound/supporting-direct3d-11-video-decoding-in-media-foundation) for device-manager setup; it does not remove the need to test D3D12 resource sharing.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T09 -->
## T09 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T02 / #3](https://github.com/Gourav1100/openps-rs/issues/3), [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T05 / #6](https://github.com/Gourav1100/openps-rs/issues/6), [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7), [T07 / #8](https://github.com/Gourav1100/openps-rs/issues/8)

### Existing code entry points

- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [scripts/build.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/build.sh#L1)

### File-level implementation

- **New `openps-video/src/decoder/linux/{mod,ffmpeg,vaapi}.rs`:** own AVCodecContext/AVPacket/AVFrame resources through RAII wrappers; implement submit/receive semantics and map EAGAIN/EOF to T03 states.
- **New `src-tauri/src/platform/linux.rs`:** native gameplay host and surface lifecycle for both Wayland and X11; share overlay/session semantics with Windows rather than sharing native pointers.
- **New `packaging/linux/` and `docs/testing/linux-matrix.md`:** declare codec/library versions, selected distribution baseline, GPU access and tested packaging modes.
- **`openps-core/src/netlink.rs`:** use the T02 portable fallback first; optionally supply native Linux route/link facts without spawning macOS commands or treating PHY rate as measured throughput.

### Execution order / PR slices

1. Software H.264/HEVC decode → CPU frame → renderer upload gives a deterministic reference path and catches timestamp/color bugs.
2. Add VA-API device/context creation, decode surface export and T05-tested DMA-BUF import. Validate plane offsets, pitch, modifiers, visible crop and physical-device identity before accepting the path.
3. Complete Wayland/X11 surface integration, T10 audio and T11 capture; then package and qualify additional hardware paths. Do not make NVIDIA acceleration a claim based only on FFmpeg build options.

### Decisions and invariants

Keep FFmpeg version/licensing/build flags explicit; choose dynamic or packaged libraries through an ADR and include notices. Avoid unsafe frame data access until pixel format and plane bounds are validated. Keep native AVFrame references alive until GPU release; on a failed modifier/import path, transfer/copy into an accepted format or fall back to software. A display/compositor change cannot reuse stale native handles.

### Targeted validation / completion evidence

Run software decode golden tests on ordinary Linux CI and hardware tests separately for supported VA-API devices. Validate GNOME/KDE Wayland and an X11 session; test renderer fallback, audio server restart, suspend/resume and denied GPU device access. Fresh installation on the minimum distro must not depend on developer-installed codec libraries. Report source profile, output format, actual hardware decoder and import route in each result.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T10 -->
## T10 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4)

### Existing code entry points

- [crates/openps-audio/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/lib.rs#L1)
- [crates/openps-audio/src/opus.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/opus.rs#L1)
- [crates/openps-audio/src/player.rs:88](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/player.rs#L88)
- [crates/openps-audio/src/media.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/src/media.rs#L1)

### File-level implementation

- **`openps-audio/src/lib.rs` / `opus.rs`:** expose a portable decoder interface and native/libopus implementation choices without Apple FFI in generic types. Reuse `header.rs`; validate channel/sample-count limits before allocation.
- **New `openps-audio/src/{jitter,ring,clock}.rs`:** move buffer policy from `player.rs` into testable code. Define `AudioClockSnapshot { frames_played, monotonic_time, device_latency, valid }` and a bounded PCM SPSC ring.
- **New `output/{mod,apple,wasapi,linux,android}.rs`:** lifecycle/output adapter contract with configure/start/stop, route-change notification and volume. Oboe requires a small maintained C ABI/JNI boundary if no suitable Rust binding meets requirements.
- **`media.rs` / shell platform factory:** decode on a worker, submit PCM and expose status without invoking UI/logging inside the audio callback.

### Execution order / PR slices

1. Extract jitter/PCM ring policy and test using a fake output clock. Preserve Apple defaults initially: 40 ms prefill, 60 ms target, 150 ms maximum.
2. Add portable Opus plus WASAPI/Linux outputs, then Android/iOS lifecycle adapters.
3. Add drift estimation and gradual resampling after route-specific baseline measurements. Keep packet-loss concealment distinct from silence caused by device interruptions.

### Decisions and invariants

Callbacks allocate nothing, take no blocking locks and never call the decoder. Count all zero-fill reasons separately: prefill, underrun, unavailable route and producer contention. Keep a bounded target latency, but do not chase every jitter excursion with abrupt sample drops. Flush and re-anchor clocks on route/sample-rate change. Bluetooth's device delay is not represented by ring occupancy alone. iOS session configuration and Android audio focus belong to native lifecycle adapters.

### Targeted validation / completion evidence

Golden PCM decode, malformed Opus, wraparound ring tests, starvation/overrun and ±clock-drift simulations precede real playback. Test headphone/HDMI/Bluetooth changes and interruptions while reconnecting. Measure A/V offset using correlated audio/video markers, not timestamps from unrelated clock domains. [Oboe](https://developer.android.com/games/sdk/oboe) is the recommended Android starting point; actual latency still needs measurement. Keep `cargo test -p openps-audio` as a base and add platform-specific integration fixtures.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T11 -->
## T11 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T02 / #3](https://github.com/Gourav1100/openps-rs/issues/3), [T04 / #5](https://github.com/Gourav1100/openps-rs/issues/5)

### Existing code entry points

- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [crates/openps-input/src/mapping.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/mapping.rs#L1)
- [crates/openps-macinput/src/hub.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-macinput/src/hub.rs#L1)
- [crates/openps-macinput/src/runner.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-macinput/src/runner.rs#L1)

### File-level implementation

- **Move reusable parts of `openps-macinput/src/hub.rs` and `runner.rs` into proposed `openps-input-host/src/{hub,runner,event,capture}.rs`.** Preserve `Engine<Platform>` and its existing `InputSnapshot`/hotkey semantics. Leave Cursor/CoreGraphics and macOS target-app operations in macinput.
- **Define proposed `HostEvent`:** physical key transition, mouse button, relative delta, wheel, focus, device removal and capture confirmation. Include device identity and monotonic event time; key repeats must not synthesize repeated button-down edges.
- **Add `adapters/windows.rs`, `linux_wayland.rs`, `linux_x11.rs`:** native event normalization and capture requests with observed state callbacks.
- **`src-tauri/src/input.rs`:** install the selected adapter and filter DOM keyboard/mouse events while native gameplay owns input; leave text-editing events in the UI path.

### Execution order / PR slices

1. Extract hub/runner with a fake capture adapter; port existing runner/cursor-lurch tests before adding new OS code.
2. Add Windows Raw Input and X11 capture. Add Wayland relative-pointer and constraints without global warp assumptions.
3. Integrate focus and lifecycle with `Engine::shutdown`, release-all and neutral pad output; add mobile-compatible capability methods while retaining desktop target-app operations as optional capabilities.

### Proposed state and scheduling

Capture request is asynchronous: `Released → Requested → Captured`; failures enter Released/Unavailable, while focus loss goes through neutralization before Suspended. Use an atomic accumulated-delta exchange and bounded ordered key/button transitions. Do not collapse press→release into one latest key state if no engine tick occurred between them. The runner currently drains `rx.try_iter()`; bound work per tick so an event flood cannot indefinitely delay `Engine::tick` or shutdown.

### Targeted validation / completion evidence

Fake event replays cover repeat, modifiers, cross-device release, focus loss, fast press/release, wheel and capture rejection. Real-device tests include multiple keyboard layouts and continuous edge-free mouse motion. A release must clear deltas and all old held inputs before any reacquire. Preserve mapper/config tests and verify UI text fields do not leak keystrokes to the console. Controller arbitration stays in T23, using this host's output contract.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T12 -->
## T12 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T05 / #6](https://github.com/Gourav1100/openps-rs/issues/6), [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7), [T07 / #8](https://github.com/Gourav1100/openps-rs/issues/8), [T10 / #11](https://github.com/Gourav1100/openps-rs/issues/11)

### Existing code entry points

- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [src-tauri/tauri.conf.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/tauri.conf.json#L1)
- [src-tauri/Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/Cargo.toml#L1)
- [crates/openps-video/src/pipeline.rs:55](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/pipeline.rs#L55)

### File-level implementation

- **Create proposed `src-tauri/plugins/openps-platform/` using Tauri's mobile-plugin structure.** Kotlin files `OpenPsPlatformPlugin.kt`, `GameplaySurface.kt`, `CodecBridge.kt`, `LifecycleBridge.kt` own platform objects. Rust `android.rs` exposes opaque session/surface handles and bounded message queues.
- **New `openps-video/src/decoder/android.rs`:** implement MediaCodec configuration/events through a narrow Kotlin or NDK bridge; choose one control owner to prevent concurrent lifecycle operations.
- **Generated Android project:** commit supported configuration templates/customizations and reproducible regeneration steps rather than relying on untracked manual edits. Native library ABIs, min SDK and NDK version are explicit.
- **`src-tauri/src/lib.rs` / platform factory:** use native gameplay install/show/hide, retaining Tauri library/settings commands; no frame pixels in IPC.

### Execution order / PR slices

1. Show a native Surface behind/alongside the webview and verify touch/focus/overlay behavior on a physical phone. If composition is unreliable, use a dedicated gameplay Activity/view with a documented return path.
2. Decode H.264 to a direct-display surface as the basic path; wire audio and stream stop/resume.
3. Add GLES SurfaceTexture processing, then qualified AHardwareBuffer/Vulkan processing from T05. Treat changing surface mode as a controlled decoder restart if required.
4. Finish lifecycle, network handoff and packaging; T13 adds capture/orientation semantics to this host.

### Decisions and invariants

Each surface has an epoch; `surfaceDestroyed` invalidates further submissions before releasing ANativeWindow. Bound outstanding codec/images and release every output buffer on error/cancellation. Configure low-latency mode only after capability checks. Keep JNI global refs scoped to owner lifetime, attach/detach worker threads correctly, and copy only small event metadata through the bridge. Stop frame production when backgrounded; do not keep rendering into a dead surface.

### Targeted validation / completion evidence

Test 100 surface recreate/pause/resume cycles, rotation/size change, process restoration, low-memory callback and audio interruption. Check output crop/color metadata and image ownership with frame-number patterns. Compile required ABIs and validate 16 KB-page compatibility of every bundled native library. [Tauri mobile-plugin guidance](https://v2.tauri.app/develop/plugins/develop-mobile/) is the shell extension point; media behavior requires physical-device tests.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T13 -->
## T13 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T11 / #12](https://github.com/Gourav1100/openps-rs/issues/12), [T12 / #13](https://github.com/Gourav1100/openps-rs/issues/13)

### Existing code entry points

- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [src-tauri/src/input.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/input.rs#L1)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)
- [dist/style.css:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/style.css#L1)

### File-level implementation

- **Proposed Kotlin `GameplaySurface.kt` / `InputBridge.kt`:** focusable gameplay view calls `requestPointerCapture()` only during a valid focused capture action. `onPointerCaptureChange()` drives effective state; `onCapturedPointerEvent()` forwards relative movement/buttons and native key handlers forward physical transitions.
- **`openps-input-host/src/capture.rs` / event normalization from T11:** accumulate deltas with a defined axis/sign convention and map Android key codes/scan codes to existing `Key`. Do not convert gameplay bindings through Unicode characters.
- **Android Activity/manifest configuration:** request landscape behavior; expose optional fixed-left/fixed-right preference. On configuration/window-size changes recompute content rectangle and input transform instead of reconnecting the console unnecessarily.
- **`dist/app.js`, `index.html`, `style.css`:** add a native-capture status/release control and letterboxed landscape viewport; keep controls usable when the OS imposes a portrait-shaped window.

### Execution order / PR slices

1. Implement native capture without the stream, forwarding a test relative-motion counter. Verify acquire/release/rejection before mapping controls.
2. Connect T11 mapper and neutralization on focus loss, pause, device disconnect and session termination. Suppress duplicate DOM gameplay delivery.
3. Integrate landscape viewport, safe insets and resize/fold coordinate transforms; test text-editing mode separately.

### Decisions and invariants

A request is not proof of capture. OS refusal must leave the UI released/unavailable. Keep system Back/Home/notification navigation and a visible release control. In captured mode relative mouse axes already describe movement; do not rotate/scale them a second time as though they were absolute touch coordinates. Absolute touch mapping uses inverse viewport transform and excludes letterbox bars. Ignore autorepeat for held pad buttons while allowing menu/text repeat policy.

### Targeted validation / completion evidence

Test Bluetooth/USB devices, modifiers, international layouts, fast unplug/reconnect, scroll and high-rate movement. Run 20 minutes without pointer-edge saturation, then open a system modal and confirm neutral output. Test Android phone rotation, tablet split-window, fold/unfold and desktop mode. Record OS overrides rather than claiming an enforceable global orientation lock. [Android pointer capture](https://developer.android.com/develop/ui/views/touch-and-input/gestures/movement) requires a focused view and is available from API 26.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T14 -->
## T14 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T12 / #13](https://github.com/Gourav1100/openps-rs/issues/13), [T13 / #14](https://github.com/Gourav1100/openps-rs/issues/14), [T23 / #24](https://github.com/Gourav1100/openps-rs/issues/24)

### Existing code entry points

- [dist/index.html:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/index.html#L1)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)
- [dist/style.css:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/style.css#L1)
- [src-tauri/tauri.conf.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/tauri.conf.json#L1)

### File-level implementation

- **Android manifest/flavor configuration (proposed `packaging/android/tv/` templates):** add Leanback launch intent, banner assets and touchscreen-not-required declarations; select TV mode from actual device capabilities, not viewport width alone.
- **New `dist/tv-navigation.js` or a small module within existing `app.js`:** deterministic focus graph, roving focus, Select/Back handling, modal focus restoration and repeat throttling. Keep it integrated with current page state rather than introducing a second router.
- **`dist/index.html` / `style.css`:** large focus targets, explicit focused styling, readable form labels and overscan-safe margins. Add console/pairing/sign-in input paths usable with remote-only hardware.
- **Platform settings defaults:** native resolution, FG off and conservative scene tier for TV unless qualification proves higher settings.

### Execution order / PR slices

1. Make existing library/settings/pairing pages traversable with D-pad and Select in desktop tests; no hover-only actions.
2. Package the TV launch target and integrate Back/release/disconnect with the Android host and T23 controller arbitration.
3. Qualify basic playback on a low-memory TV dongle and a higher-tier device; then enable optional effects per measured device capability.

### Decisions and invariants

Remote Back first closes a modal/overlay, then releases capture or opens a disconnect confirmation according to context; it must never be swallowed indefinitely. Restore focus when async discovery updates a list, and choose the nearest valid item if the focused console disappears. Sign-in must use the actual supported authentication protocol; do not invent a companion authorization endpoint. Validate soft-keyboard entry or a supported native input flow for codes.

### Targeted validation / completion evidence

Add navigation tests for every page, nested dialogs, disabled controls and async list refresh. On TV hardware, complete fresh install→pair→connect→settings→disconnect without touch/mouse/F-keys. Test HDMI/audio hotplug, sleep, Bluetooth reconnect and 59.94/60 Hz. Inspect [TV manifest requirements](https://developer.android.com/training/tv/get-started/create). Keep T29 integration as a delivery dependency even though the original issue lists only media/input dependencies. A higher-resolution panel must not cause an automatic 4K processing allocation on weak hardware.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T15 -->
## T15 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T02 / #3](https://github.com/Gourav1100/openps-rs/issues/3), [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T04 / #5](https://github.com/Gourav1100/openps-rs/issues/5), [T10 / #11](https://github.com/Gourav1100/openps-rs/issues/11)

### Existing code entry points

- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-video/build.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/build.rs#L1)
- [crates/openps-metal/build.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/build.rs#L1)
- [crates/openps-audio/build.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-audio/build.rs#L1)

### File-level implementation

- **Proposed Swift files `OpenPsPlatformPlugin.swift`, `GameplayViewController.swift`, `MetalVideoView.swift`, `AudioSessionCoordinator.swift`:** own UIKit/scene lifecycle, CAMetalLayer sizing, orientation hooks and audio interruptions.
- **`openps-video/src/vt.rs` and build configuration:** share Apple decoding where API availability allows, gate iOS support explicitly, and replace desktop-only paths with target adapters.
- **`openps-metal/src/renderer.rs` / `lib.rs`:** isolate AppKit display/window queries; receive drawable size, scale, refresh and lifecycle through the portable host. Keep shader/resource logic reusable, with runtime feature checks for MetalFX.
- **`src-tauri/src/platform/ios.rs` / mobile plugin (proposed):** opaque surface/session ownership, native authentication redirect and foreground/background events. Storage uses T28 app-private paths.

### Execution order / PR slices

1. Compile an iOS device/simulator shell and present a synthetic Metal pattern from a native UIView; verify transparent overlay or choose a dedicated gameplay controller.
2. Add VT frames through portable T03 contracts and audio through T10; preserve frame leases until GPU completion.
3. Integrate real session connect/stop, local-network permission states and interruption/reconnect behavior. T16 implements physical keyboard/mouse and final landscape policy using the controller hooks.

### Decisions and invariants

UIViews and controller mutations occur on the main thread; decoder/GPU worker callbacks carry epoch tokens and cannot message a deallocated view. Use the active UIWindowScene's geometry rather than `UIScreen.main` assumptions. On background, neutralize input first, invalidate media work and stop active presentation. On resume, recreate only necessary native objects, clear histories and request a fresh IDR through existing throttle. iOS AVAudioSession setup must precede starting output.

### Targeted validation / completion evidence

Test physical iPhone and iPad separately from simulators: first frame, foreground/background, screen lock, calls/audio interruptions, low memory, resized windows and Bluetooth audio. Verify local-network denial leads to recovery/manual-address UI, not indefinite discovery. Audit discovery broadcast/multicast API/entitlement needs before promising automatic discovery everywhere. Document MetalFX capability per tested OS/device and retain native Metal/basic rendering if unavailable. The plugin boundary follows [Tauri's native mobile extension model](https://v2.tauri.app/develop/plugins/develop-mobile/).

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T16 -->
## T16 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T11 / #12](https://github.com/Gourav1100/openps-rs/issues/12), [T15 / #16](https://github.com/Gourav1100/openps-rs/issues/16)

### Existing code entry points

- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [src-tauri/src/input.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/input.rs#L1)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)
- [dist/style.css:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/style.css#L1)

### File-level implementation

- **Proposed Swift `InputCoordinator.swift`:** register GCKeyboard/GCMouse connect/disconnect notifications; attach native key/button/movement handlers; forward normalized events to Rust without webview polling.
- **`GameplayViewController.swift`:** override pointer-lock preference while focused gameplay capture is requested and request preference updates. Keep modal/scene activation hooks so native release invalidates mapping state.
- **`openps-input-host/src/capture.rs`:** consume requested/effective/native-availability status separately; source-specific key/button release and delta clear use the same logic as desktop/Android.
- **Apple Info.plist/project template plus gameplay controller:** declare landscape orientations, availability-gate scene orientation-lock preferences and maintain a landscape content rectangle when the window is resized.

### Execution order / PR slices

1. Native input feasibility app on the intended iPhone/iPad OS versions: show received deltas, key transitions and effective focus/capture evidence before integrating Remote Play.
2. Connect Rust mapper and add lifecycle neutralization/reacquisition with no first-sample jump.
3. Add landscape launch/presentation, safe areas, text editing and resized-window behavior; qualify hardware/device combinations.

### Decisions and invariants

`prefersPointerLocked` is a preference, not an unconditional device-support guarantee. Do not expose Captured merely because it returns true; combine native focus/connection status and supported OS behavior, and document what observation proves effectiveness. Do not invent a universal capture-change callback if the selected UIKit API does not provide one; maintain a conservative state and validate release paths on hardware. Keep user/system escape mechanisms and a visible release button. Physical key bindings remain separate from UI text composition and OS-reserved shortcuts.

### Targeted validation / completion evidence

Test Bluetooth/USB keyboard/mouse, modifier combinations, sustained relative movement, device removal, alert presentation, app switching, lock/unlock and rapid play/release. Demonstrate held keys become neutral before focus transfers. Test both landscape directions and resizable iPad windows, and document unsupported iPhone combinations. Prefer fallback navigation/controller/touch over an inaccurate locked indicator. API references: [UIKit pointer-lock preference](https://developer.apple.com/documentation/uikit/uiviewcontroller/preferspointerlocked), [Apple native mouse guidance](https://developer.apple.com/videos/play/wwdc2020/10094/).

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T17 -->
## T17 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7), [T07 / #8](https://github.com/Gourav1100/openps-rs/issues/8), [T15 / #16](https://github.com/Gourav1100/openps-rs/issues/16)

### Existing code entry points

- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/tests/fsr_compare.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/fsr_compare.rs#L1)
- [crates/openps-metal/tests/video_quality.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/video_quality.rs#L1)

### File-level implementation

- **`openps-metal/src/gpu.rs::{upscale,upscale_target,rcas_lobe}` and `shaders.rs`:** extract a written math/parameter contract, preserving existing controls and edge behavior. Do not treat a port's visual similarity as proof that coefficient/range semantics match.
- **New `openps-render/src/{color,spatial}.rs`:** CPU reference for color matrices/range, crop geometry, scale/sharpen coefficients and allocation dimensions. Keep UI percentages converted once into validated parameters.
- **New WGSL/GLSL shader assets in T06/T07 backend crates:** NV12 conversion, EASU, RCAS and finish pass variants; compile variants for supported storage/render formats.
- **Existing `fsr_compare.rs`, `video_quality.rs`, `tester_gpu.rs`:** turn reusable cases into backend-independent fixture manifests and extend with color bars/padded planes.

### Execution order / PR slices

1. Verify native-size conversion/blit against reference; cover black/white levels, RGB order and limited/full range before scaling.
2. Port EASU with no sharpening, then RCAS, then optional saturation/linear-light finishing. Preserve Metal as an oracle while also comparing to a CPU/reference formulation.
3. Integrate output-size clamping, pooling and backend capability reasons in settings. HDR/P010 is a separate gated slice after SDR correctness.

### Decisions and invariants

Decoded color metadata controls conversion; missing metadata uses a documented negotiated/default policy, not a random matrix. A scale factor is constrained by visible source rectangle and drawable dimensions; downscaling uses an appropriate path rather than applying an enlargement kernel blindly. Never sharpen twice because both upscaler and final pass assume ownership. Inputs/outputs state their transfer function so enabling linear-light processing does not double-apply gamma.

### Targeted validation / completion evidence

Test impulse, diagonal edges, checkerboard, colored text, subtitles, scene gradients and clipped highlights across D3D12/Vulkan/GL/GLES/Metal. Declare absolute/perceptual tolerances and compare cropped visible areas. Benchmark each pass separately and total display path; feature disable must retain display. FSR spatial uses color-only inputs, unlike temporal reconstruction; keep [AMD spatial-upscaling terminology](https://gpuopen.com/manuals/fidelityfx_sdk/techniques/super-resolution-spatial/) accurate in UI/docs.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T18 -->
## T18 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T04 / #5](https://github.com/Gourav1100/openps-rs/issues/5), [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7), [T07 / #8](https://github.com/Gourav1100/openps-rs/issues/8), [T17 / #18](https://github.com/Gourav1100/openps-rs/issues/18), [T19 / #20](https://github.com/Gourav1100/openps-rs/issues/20)

### Existing code entry points

- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/src/camera.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/camera.rs#L1)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)

### File-level implementation

- **`gpu.rs::{ingest,compose,compose_interp,motion_ready,reset_stream}` plus existing motion shaders:** document current pass order, motion units, frame-pair identity and interpolation/extrapolation bounds before porting.
- **New `openps-render/src/{motion,cut,framegen}.rs`:** portable control/parameters; `FramePair` carries two real FrameKeys and times. `CutDecision` carries confidence/reason and triggers history invalidation.
- **WGSL/GLSL compute assets:** luma pyramid, block search/refinement, consistency/confidence, warping and disocclusion fallback. Implement per-backend kernels only for qualified feature tiers.
- **`renderer.rs` and portable renderer implementations:** generate from real-frame history only, composite local overlays afterward, and expose exact generated/repeated/real counts.

### Execution order / PR slices

1. Port motion estimation and compare vector fields/reference outputs without presenting synthetic frames.
2. Add 2× interpolation with correct source lookahead, then extrapolation with bounded prediction horizon and confidence fallback.
3. Add robust cuts/stall/discontinuity reset, HUD conservatism, camera-input prior, and integration with T19 pacing/budget. Fractional/higher multipliers are later slices, not prerequisites.

### Decisions and invariants

Do not share semantic scene classification with shot-cut detection: semantic inference is sampled and too slow to protect every frame pair. Reject mismatched epochs/formats or non-increasing pair times. Clamp predicted regions and show the latest real frame where flow is unreliable; disable generation for hard cuts and prolonged missing data. Reuse motion analysis across generated frames, but never retain an expired decoder surface without a lease. Local cursor/UI is not warped; console-baked HUD separation is approximate and needs artifact tests.

### Targeted validation / completion evidence

Extend `camera_motion`, `periodic`, `real_clip`, `replay` and video-quality cases with pans, disocclusions, fine repetitive patterns, flash/fade/cut, static HUD and scene gaps. Report held-out visual artifacts and p95 latency alongside FPS. Basic GL/GLES remains supported with FG unavailable if compute requirements fail. Keep custom video FG distinct from [FSR 3's required engine depth/motion inputs](https://gpuopen.com/fidelityfx-super-resolution-3/). Do not claim generated frames advance console simulation or improve remote input latency.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T19 -->
## T19 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T01 / #2](https://github.com/Gourav1100/openps-rs/issues/2), [T04 / #5](https://github.com/Gourav1100/openps-rs/issues/5), [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7)

### Existing code entry points

- [crates/openps-metal/src/timeline.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/timeline.rs#L1)
- [crates/openps-metal/src/ladder.rs:22](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/ladder.rs#L22)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)

### File-level implementation

- **New `openps-render/src/{pacing,metrics,budget}.rs`:** clock-domain conversion, bounded frame deadline selection, percentile/queue metrics and thermal/quality state machine.
- **`timeline.rs` / `ladder.rs`:** consume actual display observations and source/content cadence. Retain existing jitter/duplicate semantics; make policy change tests explicit rather than changing extraction tests in T04.
- **Native adapters:** supply display-refresh/present observations and thermal/power notifications; use native presentation mechanisms where reliable, with a conservative timer/vsync fallback.
- **`src-tauri/src/video.rs::stats` / `dist/app.js`:** carry validity/provenance flags and requested/effective settings. Keep diagnostics sampling bounded and separate from render timing.

### Execution order / PR slices

1. Add correlated stage stamps from T03 and a fake display clock; observe current behavior without changing scheduling.
2. Replace fixed sleep/spin decisions per backend with a deadline/pacing adapter. Support resize/monitor moves and 59.94/60/90/120/144 Hz.
3. Add budget controller with hysteresis/cooldown: reduce optional FG/expensive scale/inference when sustained deadline misses or thermal pressure occur, and recover gradually.

### Decisions and invariants

Use monotonic CPU time for cross-component correlation. Calibrate GPU/display timestamps if APIs support it; otherwise label metrics unavailable/estimated. Never subtract timestamps from unrelated clock domains. Maintain separate network queue, decode queue, intentional interpolation lookahead, GPU work and present delay. Preserve user preference while effective quality changes. A battery-saving policy may lower optional work; a quality preference cannot override resource safety. Resume/discontinuity clears timing history.

### Targeted validation / completion evidence

Deterministic tests cover clock drift, jitter bursts, duplicate content, source/display mismatch, missed deadlines, thermal oscillation and display-rate change. Verify no unbounded playout growth or repeated quality toggling. Use physical presentation markers/high-speed measurement where needed to validate input-to-photon; renderer submit time is not that metric. Run mobile thermal soak and desktop load tests with baseline native playback plus optional effects. Keep timing tests isolated from heavily loaded general CI and attach environment/measurement method.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T20 -->
## T20 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T12 / #13](https://github.com/Gourav1100/openps-rs/issues/13), [T15 / #16](https://github.com/Gourav1100/openps-rs/issues/16)

### Existing code entry points

- [crates/openps-scene/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/lib.rs#L1)
- [crates/openps-scene/src/encoder.rs:61](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/encoder.rs#L61)
- [crates/openps-scene/src/service.rs:126](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/service.rs#L126)
- [tools/scene-context/scenelab/export.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/export.py#L1)

### File-level implementation

- **New `openps-scene/src/encoders/{mod,coreml,onnx}.rs`:** `EncoderSpec`, `PreparedInput` and `ImageEncoder::encode` contracts. Keep `Probe`/`SceneFilter` independent of inference runtime.
- **`encoder.rs` / `service.rs`:** migrate CoreML/Vision implementation behind the interface and accept T03 frame leases/preprocessed tensors. Preserve whole-frame ScaleFill and feature order: 512 MobileCLIP + 1536 backbone.
- **New `openps-scene/src/models.rs` and `assets/model-manifests/`:** typed manifest with content hashes, tensor names/shapes/dtypes, normalization, layout, runtime format, provenance/license and probe hash. Resolve app resource/cache paths through T28.
- **`tools/scene-context/scenelab/export.py`, `features.py`, `pictures.py`:** export runtime variants and parity fixtures from the same canonical preprocessing/model recipe.

### Execution order / PR slices

1. Add manifest validation/model resolver and refactor Apple encoder without changing numerical output.
2. Export ONNX and run CPU parity; only then add execution-provider selection/capability reporting on Windows/Linux/Android.
3. Package optional model assets or explicit download/install flow with atomic hash verification; integrate T21 cancellation/freshness and T25 compact tiers later.

### Decisions and invariants

A runtime loading a graph is not proof of preprocessing or embedding equivalence. Reject manifest/probe layout mismatch before starting inference. Compile/download into temporary locations, verify complete hashes, then rename; clean only cache entries owned by this manifest family. Replace mtime/size cache keys with content identities. Resolve absent weights as Unavailable while gameplay/manual mappings remain usable. Keep runtime thread counts/accelerator use bounded so inference cannot starve video.

### Targeted validation / completion evidence

Run CPU/CoreML/ONNX on identical RGB fixtures and compare embeddings, probe probabilities and final replay decisions with explicit tolerances. Test missing/corrupt artifacts, bad tensor metadata, incompatible probe, interrupted installation and unsupported provider. Assert no model downloads occur implicitly at first connect. Select runtime accelerators from measured results and [documented execution providers](https://onnxruntime.ai/docs/execution-providers/), not provider names alone. Physical-device inference tests and model provenance evidence are required before declaring portable scene support.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T21 -->
## T21 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4)

### Existing code entry points

- [crates/openps-scene/src/service.rs:126](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/service.rs#L126)
- [crates/openps-input/src/scene.rs:199](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/scene.rs#L199)
- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)
- [crates/openps-scene/tests/service.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/tests/service.rs#L1)

### File-level implementation

- **`openps-input/src/scene.rs::SceneLink`:** add a monotonically increasing detector generation and an immutable configuration/snapshot carrying session/profile/model revisions. `set_enabled`, `set_active`, profile/layer/action configuration changes and stream restart invalidate pending results.
- **`openps-scene/src/service.rs::Picture`, `offer`, `run`:** capture generation and sample time at offer; revalidate immediately before publish. Keep the existing one-slot latest-picture policy.
- **Add `Clock`/fake encoder test seams:** inference completion can be delayed without sleeping real time. Add failure streak, next-retry time and freshness deadline with throttled diagnostics.
- **`SceneLink::publish_scene/status/layer`:** publish context/view/confidence/time/generation coherently. A single immutable snapshot (for example atomically swapped Arc) is easier to reason about than separate relaxed atomics; select implementation after input-loop cost measurement.

### Execution order / PR slices

1. Reproduce delayed-inference races with deterministic tests before changing service behavior.
2. Add generation validation and reset action/filter/pending-frame state; wire profile/session changes from engine/shell.
3. Add freshness expiration, model-load retry and error/backoff behavior with safe/manual layer selection and UI state.

### Concurrency details

An off→on transition must increment the generation even if `running()` is true again at publication time. One generation check alone is insufficient if a writer changes configuration immediately afterward: use publication/reader semantics that tag the snapshot and reject mismatched generations when consuming it. Do not use a spinning seqlock reader that can busy-loop on a preempted writer in the input path. Separate long error-note strings from hot-path state. Decide whether a short temporary failure holds the last fresh layer and when it expires; make that policy explicit and testable.

### Targeted validation / completion evidence

Delay model completion across off/on, profile change, allowed-context update, reconnect, model replacement and stop. No old-generation scene may become effective. Inject repeated inference errors and load failure then recovery; assert bounded logs/retries and stale fallback. Existing `set_active/set_enabled` already clear actions/scenes: preserve that behavior while fixing in-flight publication. Keep `service`, `pipeline`, `reference` and input scene-layer tests passing. A localized freshness patch can land early using a detector revision before the full T03 frame contract exists, then adopt shared epochs.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T22 -->
## T22 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T21 / #22](https://github.com/Gourav1100/openps-rs/issues/22), [T24 / #25](https://github.com/Gourav1100/openps-rs/issues/25)

### Existing code entry points

- [crates/openps-scene/src/probe.rs:82](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/probe.rs#L82)
- [crates/openps-scene/src/filter.rs:72](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/filter.rs#L72)
- [crates/openps-input/src/scene.rs:199](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/scene.rs#L199)
- [crates/openps-input/src/engine.rs:36](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-input/src/engine.rs#L36)

### File-level implementation

- **`probe.rs::Linear::check`, `Probe::from_json`, `context`, `view`:** validate nonempty dimension, complete unique class set, finite coefficients/scales/intercepts/temperature and embedding values. Define checked inference returning `Result`; retain internal fast paths only after validation.
- **New `openps-scene/src/decision.rs` (proposed):** calibrated probabilities plus margin/entropy, minimum dwell and freshness yield `Accept(context/view)` or `Abstain(reason)`. Separate classifier estimate from chosen mapping layer.
- **`filter.rs::Params` and profile configuration in `openps-input/src/config.rs`:** version game-specific optional filter settings with constrained ranges and a compatibility default. Keep unsupported custom values from causing numerical instability.
- **`engine.rs` / `scene.rs`:** explicit manual override and transition handling: release old bindings, recompute held state under new layer, avoid duplicate transient presses. Cutscene→menu mapping becomes a profile decision while preserving old defaults.

### Execution order / PR slices

1. Harden probe/parser math with hostile fixtures; no calibration/model behavior change yet.
2. Fit calibration/thresholds with T24 validation splits and add the standalone decision layer.
3. Integrate safe switching/manual overrides and per-game parameter migration, then evaluate final held-out replay.

### Numerical and behavioral details

Precompute `coef/scale` and adjusted bias once when the model loads; use fixed-size probability arrays and reusable scratch storage. Keep max-subtracted softmax and handle degenerate sums explicitly. Calibration must not consume final test labels. A model uncertain between on-foot and vehicle should not repeatedly remap controls; bounded hold then configured safe fallback is preferable. The current near-deterministic exit cue and 12-second lock must remain a named compatibility configuration, not silently removed.

### Targeted validation / completion evidence

Property tests assert finite normalized distributions for valid models; malformed classes, dimensions, zero/negative scales and nonfinite tensors return errors. Replay failed entry/repeated presses/involuntary exit/scripted entry/menu/cutscene and held-key transitions. Publish macro-F1, calibration, false switches/minute, abstention coverage and p95 transition delay. Accept threshold changes only with held-out evidence; optimization of small probe math alone is not a material end-to-end inference speedup claim.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T23 -->
## T23 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T11 / #12](https://github.com/Gourav1100/openps-rs/issues/12)

### Existing code entry points

- [crates/openps-gamepad/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/lib.rs#L1)
- [crates/openps-gamepad/src/hid.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/hid.rs#L1)
- [crates/openps-gamepad/src/map.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/map.rs#L1)
- [crates/openps-gamepad/src/mode.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-gamepad/src/mode.rs#L1)

### File-level implementation

- **`openps-gamepad/src/lib.rs`:** retain `PadSource`/`PadSink`, add a controller capability record and a generic controller kind/mapping path; current `PadKind` covers only DualSense/DualShock4, so do not disguise standard pads as Sony hardware.
- **New `openps-gamepad/src/{windows,linux,android,apple_mobile}.rs`:** platform event source and optional output adapters. Keep native connection handles owned by adapter, stable app IDs across events, and device-specific extras behind capability checks.
- **`mode.rs::Arbiter`:** extend current keyboard/physical ownership with explicit touch source while preserving per-console slot behavior and neutral handoff. Do not introduce a second competing arbiter in mobile UI code.
- **`src-tauri/src/pads.rs`, mobile plugin and `dist` touch overlay:** forward pad events, route output/haptics only to the current device, expose supported features and add minimal touch controls/release menu.

### Execution order / PR slices

1. Extend capabilities/generic mapping with mock tests; no platform I/O yet.
2. Implement standard buttons/sticks and disconnect for each platform; integrate arbitration with T11/T13/T16.
3. Add qualified gyro/touchpad/rumble/trigger/battery support per transport; extras are individually optional.

### Decisions and invariants

Avoid duplicate events when a controller appears through both raw HID and a platform game-controller API; define a preferred source and stable identity policy. Normalize axes/deadzones once. On source change, send neutral for old source before new source state, and clear its mouse/touch deltas. Touch pointer IDs remain stable until cancel/up; a canceled gesture releases all mapped controls. Disconnect must cancel queued output and never write haptic commands to a newly connected device reusing the same OS handle.

### Targeted validation / completion evidence

Use existing `mode`, `map`, `report`, `haptics` tests with added generic pad and touch fixtures. Test simultaneous device connections, reconnect, slot assignment, focus loss, mode toggle while a trigger is held, touch cancel and unsupported output features. Verify basic input on actual Windows/Linux/Android/iOS hardware and TV remote coexistence. Document advanced-feature gaps by USB/Bluetooth/platform API; basic platform readiness must not depend on full raw DualSense parity.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T24 -->
## T24 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T01 / #2](https://github.com/Gourav1100/openps-rs/issues/2)

### Existing code entry points

- [tools/scene-context/scenelab/evaluate.py:41](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/evaluate.py#L41)
- [tools/scene-context/scenelab/filtersim.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/filtersim.py#L1)
- [tools/scene-context/scenelab/data.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/data.py#L1)
- [tools/scene-context/dataset/recordings.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/dataset/recordings.json#L1)

### File-level implementation

- **`tools/scene-context/dataset/recordings.json` / `scenelab/data.py`:** add schema version, game/session/source-clip IDs, alternate-encode group, platform/resolution and annotation provenance. Reject ambiguous grouping during split generation.
- **New `scenelab/splits.py` and `metrics.py`:** deterministic grouped split manifests and transition/calibration metrics. Keep test manifests immutable by hash once designated final.
- **`evaluate.py::{out_of_recording,report,write_sequences}`:** retain existing recording holdout as a baseline and add game-disjoint and source-disjoint evaluations. Fit normalization/calibration inside training/validation folds only.
- **`filtersim.py` and input trace/capture tooling:** support recorded mapped-action timestamps alongside simulated actions, explicitly tagging which was used.
- **`crates/openps-scene/tests/replay.rs` / fixtures:** export probabilities, timestamped actions, generations, human transition intervals and expected policy outcomes for deterministic Rust replay.

### Execution order / PR slices

1. Land metadata migration and leakage/split tests; report gaps in the current corpus rather than inventing game IDs.
2. Add opt-in local trace capture and labeling support; align action and picture clocks with recorded offset/provenance.
3. Generate baseline reports and gate T22/T25 experiments on frozen validation/test splits.

### Decisions and invariants

Never split adjacent frames or alternate encodes of one source between train/test. Label transition intervals rather than forcing uncertain exact frames; report matching tolerance and ambiguous samples. A false mapping switch is measured in the final selected layer, not merely a classifier argmax. Compare metrics per game and overall; include recall for rare contexts and unknown-game behavior. Training logs/fixtures must exclude authentication secrets and only include footage with appropriate use rights.

### Targeted validation / completion evidence

Tests prove split disjointness, reproducibility, no fit on test data and correct transition matching on synthetic sequences. Validate time alignment using known action markers. Export one shared sequence and prove Python/Rust filter parity. Report macro-F1, per-class precision/recall, calibration error, coverage, switches/minute and transition p50/p95. Historical experiment outputs stay labeled historical; do not present them as evidence for a newly exported model. Basic corpus/schema work can start before media-platform work.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T25 -->
## T25 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T20 / #21](https://github.com/Gourav1100/openps-rs/issues/21), [T24 / #25](https://github.com/Gourav1100/openps-rs/issues/25), [T22 / #23](https://github.com/Gourav1100/openps-rs/issues/23), [T19 / #20](https://github.com/Gourav1100/openps-rs/issues/20)

### Existing code entry points

- [tools/scene-context/scenelab/pictures.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/pictures.py#L1)
- [tools/scene-context/scenelab/features.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/features.py#L1)
- [tools/scene-context/scenelab/export.py:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/tools/scene-context/scenelab/export.py#L1)
- [crates/openps-scene/src/service.rs:126](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-scene/src/service.rs#L126)

### File-level implementation

- **New `tools/scene-context/experiments/mobile_tiers.py` and structured experiment manifest:** define controlled candidates using existing `pictures.py`/`features.py`, not a parallel preprocessing implementation.
- **`scenelab/export.py`:** export compact/FP16/INT8 variants with model-specific tensor/layout/probe manifests. Retrain calibration/probe for changed encoders; never fill missing DINO features with zeros under the old manifest.
- **New `openps-scene/src/sampling.rs`:** `SamplingPolicy` chooses a bounded next-sample time from action urgency, uncertainty, inference cost and thermal budget. Preserve one pending sample and avoid a queue of costly inferences.
- **`service.rs` / T19 budget integration / T29 settings:** choose model tier at safe boundaries, expose effective tier and switching reasons, increment model generation and reset incompatible state.

### Execution order / PR slices

1. Time-box offline ablation: original two-model, single encoder, distilled student, reduced dimensions and numerical precision. Keep T24 splits fixed.
2. Benchmark finalists on representative mobile/TV hardware while decode/audio render concurrently; measure cold load, peak memory and sustained—not just single-inference—cost.
3. Integrate the winning qualified tier plus adaptive sampling and hysteresis. If none meets accuracy/cost gates, ship manual mapping on weak devices and document the result.

### Decisions and invariants

Sampling bursts after mapped actions must have a maximum frequency/duration and respect inference cancellation/freshness. A confidence-gated second encoder changes model design: train/evaluate the cascade as a cascade, not as missing features. Reduce expensive inference before it causes audio/video deadline failures. Avoid oscillating model tiers on transient load; switch only after cooldown and retain a compatible manual/base mapping during transition. Existing failed HUD crop experiments are evidence, not a reason to rerun them without a new hypothesis.

### Targeted validation / completion evidence

Compare every candidate on false layer switches, transition p95, calibration, RAM and power/thermal impact. Quantized models must pass embedding/probability/replay checks appropriate to expected numerical differences. Simulated thermal/action traces validate bounded sampling and no stale publication; physical 30-minute soak validates actual contention. Completion may be a documented no-ship decision for an experimental tier, with original model or manual mapping retained.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T26 -->
## T26 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T01 / #2](https://github.com/Gourav1100/openps-rs/issues/2), [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4), [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7)

### Existing code entry points

- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)
- [crates/openps-video/src/media.rs:16](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/media.rs#L16)
- [crates/openps-metal/src/renderer.rs:375](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/renderer.rs#L375)
- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)

### File-level implementation

- **`vt.rs::decode_avcc` and decoder callback context:** once T03 contracts exist, replace per-access-unit `VTDecompressionSessionWaitForAsynchronousFrames` with bounded asynchronous completion. Retain wait/quiesce behavior where required for teardown/rebuild.
- **`media.rs::DecodingMedia`:** remove long decoder ownership from a shared mutex hot path; worker owns decoder while media sink enqueues bounded requests and receives output/failure events.
- **`renderer.rs::{copy_in,DupGate::sample}` and `gpu.rs::ingest`:** compare native NV12 import/conversion versus BGRA; retain one-copy ring path where it releases decoder surfaces sooner. Reuse duplicate sample storage or add asynchronous tiny GPU signatures only if measured CPU locking cost justifies it.
- **Render resource planner/statistics:** expose retained decoder frames, copied bytes, sample-lock duration, queue age and in-flight texture bytes.

### Execution order / PR slices

1. Instrument without changing behavior and capture T01 baseline.
2. Ship asynchronous decode scheduling alone, with old/new path selectable for diagnosis. Stop if callback ordering/lifetime regressions appear.
3. Evaluate YUV/copy policy and duplicate-signature optimization independently; each PR includes an identical-fixture before/after report.

### Decisions and invariants

T03 owns the asynchronous interface and lifetime model; this task owns optimizing its Apple implementation. Do not remove every wait from the code: `frame_to_texture` is a test helper and the renderer's final wait protects teardown. An optimization is rejected if lower GPU time increases decoder pool pressure or total frame age. Replacing CPU signature sampling must not turn late signature results into decisions for the wrong FrameKey. Keep history reset on stream changes.

### Targeted validation / completion evidence

Inject callbacks after flush, failed submit, delayed completion and repeated decoder rebuild. Use lease counters and stress tests to prove no resource reuse/free before completion. Compare repeat classifications on still scenes, subtle motion and repeated-transport frames. Measure CPU p95, decode latency, GPU bandwidth, queue age and working set. Keep current copy path as a qualified fallback for incompatible formats/drivers and preserve all loss/IDR tests.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T27 -->
## T27 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T01 / #2](https://github.com/Gourav1100/openps-rs/issues/2), [T03 / #4](https://github.com/Gourav1100/openps-rs/issues/4)

### Existing code entry points

- [crates/openps-session/src/runner.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-session/src/runner.rs#L1)
- [crates/openps-session/src/lib.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-session/src/lib.rs#L1)
- [crates/openps-stream/src/video.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-stream/src/video.rs#L1)
- [crates/openps-stream/src/stream.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-stream/src/stream.rs#L1)

### File-level implementation

- **`openps-session/src/runner.rs::{reader_loop,Runner::recv,stop_requested,Emitter}` and `lib.rs::Commander`:** centralize enqueue/dequeue accounting, add age/byte observations and separate urgent lifecycle/release signaling from bulk ingress. The current `LinkClosed` enqueue does not increment `inbound`, while generic receive decrements it; first reproduce and fix that accounting inconsistency with a focused regression test.
- **`openps-stream/src/video.rs::{Pending::add,finish,VideoAssembler}`:** introduce bounded reusable unit/assembly scratch storage only after measuring allocation counts and total resident bytes.
- **`openps-fec/src/lib.rs::{decode,cauchy_matrix}`:** separate scalar reference from optional cached/vectorized kernels, key cache entries by dimensions and actual recovery pattern and enforce an explicit cache cap.
- **New perf/replay fixtures:** burst ingress, slow decode consumer, short button transitions, high loss and hostile frame IDs.

### Execution order / PR slices

1. Add queue accounting invariants and exact LinkClosed/failed-send tests; fix correctness independently of optimization.
2. Add age/byte bounds and urgent cancellation/neutral signaling. Preserve ordered key/button transitions and Takion feedback/history behavior; do not indiscriminately coalesce all pad commands.
3. Pool assembly buffers, then benchmark bounded matrix caching and SIMD dispatch in separate PRs.

### Decisions and invariants

The existing inbound threshold is a count guard, not a proof of low latency. Choose age/byte limits using measured bitrate and recovery needs. Dropping compressed delta frames must notify IDR recovery; arbitrary discard cannot pretend decoder continuity remains valid. A control queue can still be overwhelmed by an unbounded producer—cap it or use explicitly defined latest-state plus edge history. FEC SIMD dispatch must have a scalar fallback and never read past padded/unaligned slices.

### Targeted validation / completion evidence

Run session/stream/fec tests, fuzz/resource suites and deterministic packet replays. Assert accounting never underflows and returns to zero after shutdown; lifecycle remains prompt under flood. Compare recovered bytes bit-for-bit with scalar oracle for randomized erasures and dimensions. Report allocations/CPU/queue p95 at realistic loss rates, including zero loss. Reject caches that gain speed by allowing unbounded memory growth.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T28 -->
## T28 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T02 / #3](https://github.com/Gourav1100/openps-rs/issues/3)

### Existing code entry points

- [src-tauri/src/paths.rs:25](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/paths.rs#L25)
- [crates/openps-core/src/store.rs:255](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-core/src/store.rs#L255)
- [crates/openps-psn/src/store.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-psn/src/store.rs#L1)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)

### File-level implementation

- **`src-tauri/src/paths.rs`:** replace executable-relative default with `AppPaths { data, cache, logs, models, portable }` resolved from Tauri/native OS paths. Keep explicit desktop portable mode and OPENPS_HOME override with validation.
- **`openps-core/src/store.rs` and `openps-psn/src/store.rs`:** factor private atomic-file persistence from credential storage. Introduce a narrow platform credential-store trait using current SecretStore semantics; implement native Windows, Linux and Apple/mobile adapters.
- **`src-tauri/src/lib.rs::secret_store` and setup:** inject storage dependencies, distinguish non-secret preferences from credentials, and handle locked/unavailable credential stores without deleting data.
- **New `src-tauri/src/migration.rs` plus tests:** versioned path migration with source inventory, temporary destination, verification and durable completion marker. Update installers/uninstaller and model cache paths.

### Execution order / PR slices

1. Land AppPaths and private-file abstraction with old portable mode explicitly selectable.
2. Add credential adapters and route reads/writes through them; bootstrap existing files read-only during migration.
3. Implement migration, interrupted-resume behavior and sign-out cleanup, then update install/update docs.

### Decisions and invariants

Keep source files until destination is written, synced and verified. On Windows use native secure directory/file ACL and replacement semantics; Unix modes do not apply. Serialize writers or use unique temporary names so concurrent saves cannot delete each other's temporary files. A Linux desktop may lack an unlocked secret service; choose an explicit user-visible fallback policy rather than silently weakening storage. Mobile uses sandbox/app-private paths and platform-protected key material. Cache cleanup never traverses arbitrary user paths or removes registrations/keymaps.

### Targeted validation / completion evidence

Test read-only installation, no home/environment override, relative invalid paths, interrupted migration after each step, existing destination conflicts, concurrent save, inaccessible credential store and rollback. Verify sign-out deletes relevant credential entries across migrated stores without deleting unrelated settings. Test permission/ACL behavior on native hosts, not mocks only. Check bundled archives contain no local credentials. Migration completion evidence includes an install/update/rollback matrix and redacted sample manifests.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T29 -->
## T29 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T04 / #5](https://github.com/Gourav1100/openps-rs/issues/5), [T06 / #7](https://github.com/Gourav1100/openps-rs/issues/7), [T11 / #12](https://github.com/Gourav1100/openps-rs/issues/12), [T28 / #29](https://github.com/Gourav1100/openps-rs/issues/29)

### Existing code entry points

- [crates/openps-core/src/types.rs:167](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-core/src/types.rs#L167)
- [src-tauri/src/lib.rs:213](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/lib.rs#L213)
- [src-tauri/src/video.rs:160](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/src/video.rs#L160)
- [dist/app.js:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/dist/app.js#L1)

### File-level implementation

- **`openps-core/src/types.rs::StreamOptions` and persistence:** add schema-versioned preferences using typed enums for renderer/decoder/upscaler/FG mode and quality tier. Keep wire stream options distinct from local presentation settings so a UI renderer change does not accidentally renegotiate protocol options.
- **New portable `EffectiveSettings`/`CapabilitySnapshot`:** derive effective behavior from requested settings plus actual decoder-renderer pair, device limits and budget state; each override has a stable reason code.
- **`src-tauri/src/lib.rs` commands and `video.rs::{render_settings,stats}`:** expose capabilities/settings and preserve old command compatibility while UI migrates.
- **`dist/app.js`, `index.html`, `style.css` and UI tests:** replace Apple-specific choices with runtime capability lists, show requested/effective differences, and adapt layout for phone/tablet/TV with release control always reachable.

### Execution order / PR slices

1. Implement a pure v1→v2 settings migration with fixtures for all old `engine`, `interpolator`, `frame_mode` and `frame_gen` sentinel/value combinations.
2. Add capability/effective-settings commands while maintaining current UI behavior.
3. Change controls/help/diagnostics, then mobile and TV focus/layout integration. Test backend switching with active stream and clearly identify settings requiring decoder/surface restart.

### Decisions and invariants

Keep unknown/future values from crashing startup; preserve a backup before writing the migrated schema. Persist user intent even when a device temporarily cannot fulfill it. Backend Auto may select a working fallback; forced selection must explain failure instead of claiming the forced backend is active. The current code already documents the UI label as **Low latency** for `zero_latency`; do not introduce a redundant rename. Improve explanation of prediction versus interpolation and report actual measured/estimated latency.

### Targeted validation / completion evidence

Add migration tests, capability matrix UI tests, keyboard/D-pad focus tests and viewport layout checks. Update existing source assertions in `display-settings.test.js` that assume only MetalFX/FSR, but supplement them with behavior tests rather than merely replacing expected strings. Report real/unique/generated/presented FPS independently and mark unavailable GPU/present timing. A disabled option must explain why without losing the stored preference.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T30 -->
## T30 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T01 / #2](https://github.com/Gourav1100/openps-rs/issues/2), [T02 / #3](https://github.com/Gourav1100/openps-rs/issues/3)

### Existing code entry points

- [Cargo.toml:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/Cargo.toml#L1)
- [scripts/build.sh:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/scripts/build.sh#L1)
- [crates/openps-video/tests/pipeline.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/tests/pipeline.rs#L1)
- [crates/openps-metal/tests/replay.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/tests/replay.rs#L1)

### File-level implementation

- **New `.github/workflows/{portable,desktop,mobile,hardware}.yml`:** separate portable lint/test, native shell builds, Android/iOS compile packaging and qualified hardware replay jobs. Choose final runner/tool versions after a small dependency-build spike.
- **New `tools/ci/` scripts and `docs/testing/qualification.md`:** common commands, fixture hash verification, artifact schema, timeout/retry policy and capability-qualified test selection.
- **Existing media/render/input/scene tests:** identify portable versus native versus external-asset tests explicitly; missing hardware should report skipped/unqualified, not a fabricated pass.
- **T01 device matrix and reports:** include exact source commit, artifact hash, compiler/SDK/driver/model versions and test outcome links.

### Execution order / PR slices

1. Add fmt, clippy, portable library tests and `node --test tests/*.test.js`; establish clean-checkout reproducibility before adding release credentials.
2. Add Windows/Linux/macOS native builds and Android ABI/iOS simulator/device compile checks with pinned toolchains and cached dependencies keyed by lockfiles/target.
3. Add scheduled/manual hardware matrix, real-console checklist and soak tests as platform tasks become runnable. Do not wait for every platform before establishing CI.

### Decisions and invariants

A single `cargo test --workspace` on Linux may still include Apple-specific tests until T02 gating is complete; use an explicit audited crate/target matrix rather than hiding failures with broad ignores. Hardware jobs run on trusted triggers/hosts: untrusted pull requests must not execute against private credentials or consoles. Signing happens in a separate restricted release job. Flaky timing tests record failure evidence and bounded rerun policy, not infinite retries. External weights/footage are opt-in verified inputs.

### Targeted validation / completion evidence

Intentionally break a portable test and platform build in a trial branch to verify CI fails correctly, then restore. Assert missing required fixture/model is a clear configuration failure in jobs that require it. Validate native decoder/GPU/input jobs report actual device capabilities. A platform release requires play/audio/input, lifecycle/fallback, install and soak evidence; simulator compilation alone never qualifies pointer lock or hardware decoding. Include retention/redaction rules for logs and test captures.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T31 -->
## T31 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T08 / #9](https://github.com/Gourav1100/openps-rs/issues/9), [T09 / #10](https://github.com/Gourav1100/openps-rs/issues/10), [T12 / #13](https://github.com/Gourav1100/openps-rs/issues/13), [T14 / #15](https://github.com/Gourav1100/openps-rs/issues/15), [T15 / #16](https://github.com/Gourav1100/openps-rs/issues/16), [T16 / #17](https://github.com/Gourav1100/openps-rs/issues/17), [T28 / #29](https://github.com/Gourav1100/openps-rs/issues/29), [T29 / #30](https://github.com/Gourav1100/openps-rs/issues/30), [T30 / #31](https://github.com/Gourav1100/openps-rs/issues/31)

### Existing code entry points

- [README.md:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/README.md#L1)
- [RELEASE_NOTES.md:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/RELEASE_NOTES.md#L1)
- [docs/USAGE.md:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/docs/USAGE.md#L1)
- [src-tauri/tauri.conf.json:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/src-tauri/tauri.conf.json#L1)

### File-level implementation

- **`src-tauri/tauri.conf.json` and proposed platform config overlays:** declare per-platform artifact targets, resource/model manifests and version metadata. Avoid one global bundle configuration that accidentally includes development credentials.
- **`scripts/build.sh`, `install.sh`, `make-dmg.sh` plus proposed `packaging/windows`, `packaging/linux`, `packaging/android`, `packaging/ios`:** reproducible build/package entry points and platform prerequisites. Reuse T30 release jobs rather than maintaining a second toolchain definition.
- **`README.md`, `docs/USAGE.md`, `RELEASE_NOTES.md` and proposed `docs/platform-support.md`:** installation, native permissions, capture release, fallback behavior, data paths, test device matrix and model acquisition.
- **Proposed `THIRD_PARTY_NOTICES`/license inventory:** record Rust/native/codec/shader/model assets separately and resolve the project's missing/ambiguous licensing with maintainers before binary distribution.

### Execution order / PR slices

1. Build unsigned/internal artifacts with a manifest listing included files and hashes; scan for developer credentials and unintended large assets.
2. Run fresh install/update/uninstall and T28 migration on each platform; add signing/notarization/store packaging configuration through secured CI.
3. Reconcile docs with actual tested capabilities and publish only qualified prereleases after the release gate. This planning comment does not mark any binary as ready.

### Decisions and invariants

Keep secret signing material outside the repository and ordinary PR workflows. Resource packaging must include the exact model manifest/probe pair or clearly support model-unavailable manual mode. Fix README contradictions about constants and license based on actual repository contents and maintainer decisions; do not assume an icon license licenses the project. Replace sibling `../analysis` references with in-repo docs or explicitly unavailable historical references. Architecture labels distinguish native Vulkan, translated Vulkan, native GL and ANGLE.

### Targeted validation / completion evidence

Verify release archives on clean machines/devices, read-only install paths, offline first run, upgrade from current portable layout, rollback and user-selected data retention. Ensure app settings remain recoverable across downgrade. Publish artifact checksums, dependency/toolchain provenance and tested support table. Existing API/upscaler/FG claims must match runtime behavior, including known iOS pointer/landscape and low-tier TV limitations. Do not publish actual secret-bearing test logs or authentication captures.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


---

<!-- openps-implementation-plan-v1:T32 -->
## T32 — Detailed implementation plan

Prepared against code commit `a27431fbe84b3848d06ad3ea284d85e01773f2ef`; current main `ee47a395e12ae80811f5df4c462be6410c7ef42c` adds planning documents only. This is an implementation proposal, not completed work. Proposed new files/APIs are explicitly described below; verify exact SDK bindings during implementation.

**Specification:** [complete cross-platform spec](https://github.com/Gourav1100/openps-rs/blob/main/docs/planning/openps-cross-platform-spec.md) · **Tracking:** #1

**Prerequisites for integration:** [T15 / #16](https://github.com/Gourav1100/openps-rs/issues/16), [T17 / #18](https://github.com/Gourav1100/openps-rs/issues/18), [T18 / #19](https://github.com/Gourav1100/openps-rs/issues/19), [T30 / #31](https://github.com/Gourav1100/openps-rs/issues/31)

### Existing code entry points

- [crates/openps-metal/src/gpu.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/gpu.rs#L1)
- [crates/openps-metal/src/shaders.rs:1](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-metal/src/shaders.rs#L1)
- [crates/openps-video/src/vt.rs:436](https://github.com/Gourav1100/openps-rs/blob/a27431fbe84b3848d06ad3ea284d85e01773f2ef/crates/openps-video/src/vt.rs#L436)

### File-level implementation

- **New `tools/experiments/apple-vulkan/` (isolated, proposed):** minimal texture import→blit→spatial upscale sample using a pinned Vulkan Portability implementation. No production dependency on this experiment.
- **New `tools/experiments/video-temporal/`:** controlled video-only temporal reconstruction benchmark with the same source clips/timing/quality metrics as T17/T18; keep each algorithm's actual required inputs documented.
- **New `docs/architecture/adr-002-optional-rendering-research.md`:** question, inputs, library/license versions, device matrix, baseline, quality/latency/memory results and explicit ship/defer/reject decision.
- Production integration, if justified, is limited to a separately feature-gated backend factory entry and capability metadata. Default iOS remains Metal.

### Execution order / PR slices

1. After native iOS playback is stable, prove Vulkan Portability present and CoreVideo/Metal-backed resource import using a synthetic frame; then replay identical decoded clips through native Metal and translated Vulkan.
2. Compare binary size, build complexity, memory, startup, CPU/GPU cost, sustained thermals and driver/API limitations. Stop the experiment if it cannot safely import/retain native resources or requires unacceptable copies.
3. Evaluate temporal video reconstruction separately; distinguish optical-flow-derived estimates from engine-authored motion/depth. Report visual quality and latency against spatial scaling, not only PSNR on interpolated synthetic examples.

### Decisions and invariants

Do not promise native Vulkan on iOS or DirectX outside Windows. Shader translation support does not establish decoder-interoperability or OS distribution compatibility. Do not pass dummy depth/motion into an engine-oriented SDK and label that a valid FSR/DLSS/XeSS integration. If researching estimated depth/motion, name it an experimental video reconstruction method and evaluate it accordingly. Third-party SDK integration requires its own inputs/license/device support review.

### Targeted validation / completion evidence

One-week research time box per question followed by an ADR; negative results are useful completion. Attach reproducible commands and pinned dependencies, held-out image/video comparisons, frame-time distributions and observed failure modes. No production path changes without evidence of a material benefit over native Metal/custom video FG and a follow-up implementation decision. The existing [wgpu backend matrix](https://docs.rs/wgpu/latest/wgpu/struct.Backends.html) and [FSR integration requirements](https://gpuopen.com/fidelityfx-super-resolution-3/) describe available APIs/input expectations, not proof this experiment will work.

### Merge handoff

Attach the task-specific tests/evidence above, document any capability exclusions, and link the implementing PRs here. Preserve the issue body's acceptance criteria and rollback/fallback plan. Land the PR slices in order; do not close this issue merely because scaffolding compiles.


## Direct links to implementation comments

- [T01 — implementation comment on #2](https://github.com/Gourav1100/openps-rs/issues/2#issuecomment-6947047589)
- [T02 — implementation comment on #3](https://github.com/Gourav1100/openps-rs/issues/3#issuecomment-6947051607)
- [T03 — implementation comment on #4](https://github.com/Gourav1100/openps-rs/issues/4#issuecomment-6947052773)
- [T04 — implementation comment on #5](https://github.com/Gourav1100/openps-rs/issues/5#issuecomment-6947054527)
- [T05 — implementation comment on #6](https://github.com/Gourav1100/openps-rs/issues/6#issuecomment-6947056229)
- [T06 — implementation comment on #7](https://github.com/Gourav1100/openps-rs/issues/7#issuecomment-6947058302)
- [T07 — implementation comment on #8](https://github.com/Gourav1100/openps-rs/issues/8#issuecomment-6947059698)
- [T08 — implementation comment on #9](https://github.com/Gourav1100/openps-rs/issues/9#issuecomment-6947060939)
- [T09 — implementation comment on #10](https://github.com/Gourav1100/openps-rs/issues/10#issuecomment-6947066303)
- [T10 — implementation comment on #11](https://github.com/Gourav1100/openps-rs/issues/11#issuecomment-6947067721)
- [T11 — implementation comment on #12](https://github.com/Gourav1100/openps-rs/issues/12#issuecomment-6947069111)
- [T12 — implementation comment on #13](https://github.com/Gourav1100/openps-rs/issues/13#issuecomment-6947070494)
- [T13 — implementation comment on #14](https://github.com/Gourav1100/openps-rs/issues/14#issuecomment-6947071773)
- [T14 — implementation comment on #15](https://github.com/Gourav1100/openps-rs/issues/15#issuecomment-6947072984)
- [T15 — implementation comment on #16](https://github.com/Gourav1100/openps-rs/issues/16#issuecomment-6947074788)
- [T16 — implementation comment on #17](https://github.com/Gourav1100/openps-rs/issues/17#issuecomment-6947075962)
- [T17 — implementation comment on #18](https://github.com/Gourav1100/openps-rs/issues/18#issuecomment-6947080959)
- [T18 — implementation comment on #19](https://github.com/Gourav1100/openps-rs/issues/19#issuecomment-6947082139)
- [T19 — implementation comment on #20](https://github.com/Gourav1100/openps-rs/issues/20#issuecomment-6947084622)
- [T20 — implementation comment on #21](https://github.com/Gourav1100/openps-rs/issues/21#issuecomment-6947086125)
- [T21 — implementation comment on #22](https://github.com/Gourav1100/openps-rs/issues/22#issuecomment-6947087543)
- [T22 — implementation comment on #23](https://github.com/Gourav1100/openps-rs/issues/23#issuecomment-6947088883)
- [T23 — implementation comment on #24](https://github.com/Gourav1100/openps-rs/issues/24#issuecomment-6947090707)
- [T24 — implementation comment on #25](https://github.com/Gourav1100/openps-rs/issues/25#issuecomment-6947092112)
- [T25 — implementation comment on #26](https://github.com/Gourav1100/openps-rs/issues/26#issuecomment-6947096413)
- [T26 — implementation comment on #27](https://github.com/Gourav1100/openps-rs/issues/27#issuecomment-6947097755)
- [T27 — implementation comment on #28](https://github.com/Gourav1100/openps-rs/issues/28#issuecomment-6947100128)
- [T28 — implementation comment on #29](https://github.com/Gourav1100/openps-rs/issues/29#issuecomment-6947102077)
- [T29 — implementation comment on #30](https://github.com/Gourav1100/openps-rs/issues/30#issuecomment-6947103367)
- [T30 — implementation comment on #31](https://github.com/Gourav1100/openps-rs/issues/31#issuecomment-6947104955)
- [T31 — implementation comment on #32](https://github.com/Gourav1100/openps-rs/issues/32#issuecomment-6947106385)
- [T32 — implementation comment on #33](https://github.com/Gourav1100/openps-rs/issues/33#issuecomment-6947107688)
