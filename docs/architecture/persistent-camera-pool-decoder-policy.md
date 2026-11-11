# Persistent camera pool and per-camera decoder policy

Status: proposed architecture for the next native renderer implementation.
Target baseline: `v0.2.19-fork`. This document does not change runtime behavior.

## Goals

1. A camera used by any configured panel has one long-lived grid RTSP session.
2. Selecting another panel changes only KMS viewport composition. It must not close, reopen, or prewarm grid RTSP sessions.
3. The administrator chooses the decode path independently for each camera.
4. The total number of hardware decoder sessions is bounded. Default limit: 6.
5. A camera that cannot use its requested hardware path still falls back to software, with an explicit runtime reason.
6. The actual decoder/mode/state remains visible in `/api/status` and Home Assistant.

## Configuration contract

Add a top-level `hardware_decoder_limit` (integer, default `6`, range `0..=16`) and a `decoder` property to each feed:

```yaml
hardware_decoder_limit: 6

feeds:
  # Explicit preference: always use software for this camera.
  dvor_full:
    url: rtsp://camera/stream
    decoder: software

  # Explicit preference: reserve a hardware slot for this camera.
  ulica_vverh:
    url: rtsp://camera/stream
    decoder: hardware

  # Default for existing configurations: use an available slot, otherwise software.
  pool:
    url: rtsp://camera/stream
    decoder: auto
```

Allowed values are `hardware`, `software`, and `auto`; omitted values mean `auto` for backward compatibility.

### Deterministic slot assignment

- Explicit `hardware` cameras are allocated first.
- If explicit hardware selections exceed `hardware_decoder_limit`, configuration validation fails with the names of the conflicting cameras. It must not silently ignore a user's explicit selection.
- `auto` cameras fill remaining slots in stable feed-name order.
- `software` cameras never attempt V4L2/DRM hardware decoding.
- If a selected hardware decoder is unavailable or fails, that camera falls back to software; the failure is reported in decoder telemetry.
- Fullscreen/main-stream sessions share the same global hardware budget. They may use a free slot, but must fall back to software when all slots are occupied. A fullscreen stream must never evict a persistent grid camera from its reserved slot.

The Home Assistant camera manager will expose a per-camera selector (Automatic / Hardware / Software), and a global number control for the hardware-session limit (default 6). The camera selector is the user's priority mechanism; no hidden ranking based on camera name should override an explicit choice.

## Runtime lifecycle

### Startup

- Build one `Source` per configured grid camera, across the union of all panels.
- Start every such source once the renderer is ready.
- Assign decoder policy before spawning the feed workers so allocation is deterministic.
- A camera not in the initially selected panel still connects and publishes its latest frame into its shared `Source`.

### Panel switch

- Keep all grid sources wanted while at least one configured display is being watched.
- Do not set outgoing-only camera sources to `wanted=false`.
- Do not wait for outgoing decoder sessions to stop; they are not being stopped.
- Detach the old KMS planes, wait for the detach flip, replace viewport geometry, then commit the new panel using already-published frames.
- Reuse the same source and decoder for a camera shared by multiple panels and for a camera returning to a previously viewed panel.

Expected result: switching latency is a KMS scene transition (normally one or a few display flips), not RTSP connection + keyframe + decoder startup latency.

### Pause / display off

Existing `pause_off_screen` behavior remains respected. When every display is unwatched, all persistent grid sources may pause. When a display becomes watched again, all configured grid sources resume, not only the active panel's feeds.

### Reconfiguration

Changes to a camera URL, its decoder preference, the hardware limit, or the camera registry require the existing explicit renderer reload path. A normal panel selection must remain in-process and must not restart the renderer.

## Safety and compatibility

- Existing YAML files remain valid; missing decoder policy and hardware limit receive defaults.
- RTSP transport remains TCP.
- Keep per-camera fallback behavior and credential redaction.
- Keep the existing KMS detach/flip lifetime rules. A source may keep decoding while its frame is not on screen, but a hardware frame remains owned until the display has flipped away from it.
- Do not change Pi kernel settings, CMA, GPU memory, systemd, or boot configuration as part of this feature.
- Do not merge or publish a release until CI and code review pass.

## Acceptance tests

1. A two-panel fixture with disjoint camera sets starts every unique camera once at startup.
2. Switching between panels does not change the decoder session generation/connection count for any grid camera.
3. A camera common to multiple panels retains the exact same source and decoder.
4. With limit 6, six explicit hardware cameras are selected as hardware; all software cameras bypass hardware; auto cameras use only remaining slots.
5. Seven explicit hardware cameras with limit 6 fail validation and identify the conflicting selection.
6. Limit 0 forces all grid cameras to software.
7. A hardware open/stall failure falls back only that camera to software and does not restart other cameras.
8. Fullscreen cannot exceed the global hardware-session limit or evict a grid camera's hardware slot.
9. With `pause_off_screen`, all grid cameras pause together and all resume together.
10. Decoder telemetry distinguishes requested policy from actual decoder/mode and exposes fallback reason.

## Rollout

Implement in a feature branch and open a draft PR. Run unit/integration tests and cross-build checks first. The installed `0.2.19-fork` remains untouched until the user explicitly approves installation.
