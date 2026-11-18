# Desktop portability verification — 2026-11-18

Reviewed baseline: `ea931d1` through `468922f2e2280296305bd075ee271ad085e708b8` (v0.5.0). These implementation changes were already committed and pushed to main before this review. This PR adds review fixes and verification evidence; it does not reintroduce that code.

## Platform evidence

| Platform | Evidence | Remaining |
| --- | --- | --- |
| macOS arm64 | User reports verification. Reviewer: workspace/test compilation passed; selected Rust suite passed 808 tests, 6 ignored, 0 failures; frontend passed 46 tests. | Hardware/performance qualification across all P0 acceptance criteria is not established by these tests. |
| Windows | User reports verification. Reviewer cross-check of portable render/input/scene libraries for x86_64-pc-windows-gnu passed. | No reviewer-run Windows GUI/hardware test. Native gameplay media/input backends are absent. |
| Linux | Portable library cross-check passed; Linux aarch64 Docker workspace suite passed: 1,224 Rust tests, 10 ignored, zero failures; frontend 45 passed, one platform skip, zero failures. | Interactive Linux verification remains pending, including X11/Wayland and packaging on target distributions. |
| Android/iOS | Portable shared libraries and CI checks exist. | Native applications/media/input and physical-device qualification remain outstanding. |

## Review fixes

1. `Engine::poll_window` now synchronizes scene configuration immediately on focus changes. Previously the scene detector could remain active until the next 100 ms poll after focus was lost. A regression test proves that old inference cannot publish after loss and regain of focus. All 25 advanced-mapping tests pass.
2. `.dockerignore` now includes `build-all.sh`, which the container's frontend test suite reads. The initial Linux run failed eight tests with ENOENT before reaching Rust tests. The corrected rerun passes the entire Linux workspace test job.

## Scope reviewed

Reviewed the shared render and input extraction, scene publication/lifecycle changes, target dependency and Unix filesystem guards, UDP portability changes, capability-gated UI, PSN loading flow, Docker/CI/build scripts, and release documentation. Existing tests exercise these paths, but do not prove every device/backend acceptance criterion. The baseline also includes 495 scene dataset JPG additions and label changes; this review does not certify their semantic labels or model accuracy.

Windows/Linux currently use the stats-only media fallback in `media_and_events`; native decode/render/audio and local input are macOS-only. Do not equate desktop shell verification with playable cross-platform streaming.

## P0 acceptance audit

All 19 tasks are marked **needs verification** and remain open. This label requests acceptance review; it is not a claim that implementation is complete.

| Issue | Current finding |
| --- | --- |
| #2 | Replay/timing tests exist, but measured hardware performance and release qualification remain incomplete. |
| #3 | Desktop compile boundaries and capability gating are present; Linux runtime verification is pending. |
| #4 | Owned cross-platform frame/decoder interfaces and media epochs are not implemented. |
| #5 | Portable render policy is extracted and regression-tested; native integration qualification remains. |
| #6 | Decoder/GPU interop prototypes and measurements are not implemented. |
| #7 | Vulkan/D3D12 presentation and processing backends are not implemented. |
| #9 | Windows shell is user-verified; Media Foundation decoding and native gameplay host remain unimplemented. |
| #10 | Linux shell/build infrastructure exists; native decode, Wayland/X11 presentation and interactive verification remain pending. |
| #11 | Portable Opus/output backends and independent A/V policy remain unimplemented. |
| #12 | Portable input host is extracted; Windows/Linux capture adapters remain unimplemented. |
| #13 | Android native shell, MediaCodec and lifecycle remain unimplemented. |
| #14 | Android pointer capture, keyboard and landscape gameplay remain unimplemented. |
| #15 | Android TV launcher, remote UX and device qualification remain unimplemented. |
| #16 | iOS native media host and sandbox lifecycle remain unimplemented. |
| #17 | iOS keyboard/relative mouse capture and landscape controller remain unimplemented. |
| #22 | Generation checks, freshness fallback and inference-error recovery are implemented. Review fixes immediate focus invalidation; hardware stress verification and model reload/recovery coverage remain. |
| #24 | Existing controller arbitration is preserved; new platform controller/touch backends remain unimplemented. |
| #29 | Unix guards and Windows sidecar locking exist; native credential protection, paths and migration are incomplete. |
| #31 | CI matrix and Docker jobs exist; Linux runtime and hardware/media/GPU conformance remain pending. |

## Reproduce

```sh
mkdir -p target/tmp
node --test tests/*.test.js
cargo check --locked --workspace --tests
cargo test --locked -p openps-render -p openps-input -p openps-input-host -p openps-scene -p openps-core -p openps-psn -p openps-gamepad -p openps-app --features psn-live -- --test-threads=2
cargo test --locked -p openps-input --test advanced
docker compose run --build --rm linux-test
docker compose run --build --rm linux-build
```

If using the existing Colima build VM and standalone Compose: prefix commands with `DOCKER_CONTEXT=colima-openps-build docker-compose` instead of `docker compose`.

Linux manual gate: launch the resulting executable on supported Linux distributions under both Wayland and X11; verify UI, sign-in/cancel/retry, discovery/pairing persistence, restart, writable/read-only install locations and capability gating. Record distribution, architecture, display server and artifact commit. Streaming backend tasks require their own media/input implementation and hardware evidence before closure.

## Remaining review concerns

- Model-load recovery retries the originally resolved model path; a service started with no discovered models cannot discover a newly installed model until it is recreated. #22 needs recovery coverage for model installation and repeated encoder failures.
- Native credential stores and migration-safe OS data paths are still outstanding (#29); Unix permission guards alone do not fulfill that task.
- No measured scene-detection accuracy improvement or GPU/backend performance qualification is claimed.
