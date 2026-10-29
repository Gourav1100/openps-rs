//! Metal Shading Language, compiled at run time (`newLibraryWithSource`): no offline Metal toolchain is needed.

pub const SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

constant float3 LUMA = float3(0.2126, 0.7152, 0.0722);

// ---- 1. luma at half resolution ----------------------------------------------------------------------------
kernel void luma_half(texture2d<float, access::sample> src [[texture(0)]],
                      texture2d<float, access::write>  dst [[texture(1)]],
                      uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= dst.get_width() || gid.y >= dst.get_height()) return;
    constexpr sampler s(coord::pixel, filter::linear, address::clamp_to_edge);
    // the centre of a 2x2 footprint: the linear filter averages the four pixels
    float3 c = src.sample(s, float2(gid) * 2.0 + 1.0).rgb;
    dst.write(float4(dot(c, LUMA), 0, 0, 1), gid);
}

// ---- 1b. did the picture change? -----------------------------------------------------------------------------------------
// Counts, over every second pixel of both axes of the half-size luma, the pixels that differ by more than ~1.5 % from the previous frame.
// A repeated picture (the console sends the stream's rate whatever the game renders) leaves the count near zero.
kernel void frame_diff(texture2d<float, access::read> cur  [[texture(0)]],
                       texture2d<float, access::read> prev [[texture(1)]],
                       device atomic_uint* out [[buffer(0)]],
                       uint2 gid [[thread_position_in_grid]]) {
    uint2 p = gid * 2;
    if (p.x >= cur.get_width() || p.y >= cur.get_height()) return;
    if (abs(cur.read(p).r - prev.read(p).r) > 0.015) atomic_fetch_add_explicit(out, 1, memory_order_relaxed);
}

// ---- 2. block motion search ----------------------------------------------------------------------------------
struct MEParams { int radius; float lambda; float flat; float confScale; };
struct Stats { atomic_uint bad; atomic_uint total; };
struct StatsRead { uint bad; uint total; };
inline bool scene_cut(device StatsRead* s) { return s->total > 0 && s->bad * 100 > s->total * 55; }

kernel void reset_stats(device Stats* s [[buffer(0)]]) {
    atomic_store_explicit(&s->bad, 0, memory_order_relaxed);
    atomic_store_explicit(&s->total, 0, memory_order_relaxed);
}

// One thread per 8x8 block (in half-resolution pixels). Finds where the block of `cur` came from in `prev` and stores the
// motion prev -> cur in half-resolution pixels per frame, with a confidence in 0...1.
kernel void motion(texture2d<float, access::read>  cur  [[texture(0)]],
                   texture2d<float, access::read>  prev [[texture(1)]],
                   texture2d<float, access::write> mv   [[texture(2)]],
                   constant MEParams& P [[buffer(0)]],
                   device Stats* stats [[buffer(1)]],
                   uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= mv.get_width() || gid.y >= mv.get_height()) return;
    atomic_fetch_add_explicit(&stats->total, 1, memory_order_relaxed);
    const int B = 8;
    int2 org = int2(gid) * B;
    int2 hi = int2(int(cur.get_width()) - 1, int(cur.get_height()) - 1);
    float cb[64];
    for (int j = 0; j < B; j++)
        for (int i = 0; i < B; i++)
            cb[j * B + i] = cur.read(uint2(clamp(org + int2(i, j), int2(0), hi))).r;

    float zero = 0.0;
    for (int j = 0; j < B; j++)
        for (int i = 0; i < B; i++)
            zero += abs(cb[j * B + i] - prev.read(uint2(clamp(org + int2(i, j), int2(0), hi))).r);

    // static content (and a still image, the HUD...): no motion, full confidence
    if (zero < P.flat * 64.0) { mv.write(float4(0, 0, 1, 0), gid); return; }

    float best = zero;
    int2 bd = int2(0);
    for (int dy = -P.radius; dy <= P.radius; dy++) {
        for (int dx = -P.radius; dx <= P.radius; dx++) {
            if (dx == 0 && dy == 0) continue;
            float s = P.lambda * float(abs(dx) + abs(dy));   // prefer the shorter vector among near-equal matches
            for (int j = 0; j < B && s < best; j++)
                for (int i = 0; i < B; i++)
                    s += abs(cb[j * B + i] - prev.read(uint2(clamp(org + int2(i + dx, j + dy), int2(0), hi))).r);
            if (s < best) { best = s; bd = int2(dx, dy); }
        }
    }
    float conf = saturate(1.0 - (best / 64.0) * P.confScale);
    if (best / 64.0 > 0.08) atomic_fetch_add_explicit(&stats->bad, 1, memory_order_relaxed);
    mv.write(float4(float(-bd.x), float(-bd.y), conf, 0), gid);
}

// ---- 2b. refinement: 4x4 blocks, candidates from the neighbouring coarse blocks ------------------------------
// Coarse blocks straddle object boundaries; trying the neighbours' vectors (plus a one-pixel search around each) lets each
// small block pick the motion of the object it actually belongs to.
kernel void refine(texture2d<float, access::read>  cur  [[texture(0)]],
                   texture2d<float, access::read>  prev [[texture(1)]],
                   texture2d<float, access::read>  mv0  [[texture(2)]],
                   texture2d<float, access::write> mv1  [[texture(3)]],
                   constant MEParams& P [[buffer(0)]],
                   uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= mv1.get_width() || gid.y >= mv1.get_height()) return;
    const int B = 4;
    int2 org = int2(gid) * B;
    int2 hi = int2(int(cur.get_width()) - 1, int(cur.get_height()) - 1);
    int2 chi = int2(int(mv0.get_width()) - 1, int(mv0.get_height()) - 1);
    float cb[16];
    for (int j = 0; j < B; j++)
        for (int i = 0; i < B; i++)
            cb[j * B + i] = cur.read(uint2(clamp(org + int2(i, j), int2(0), hi))).r;
    int2 pc = int2(gid) / 2;
    float2 parent = mv0.read(uint2(clamp(pc, int2(0), chi))).xy;
    float best = 1e9;
    float2 bm = float2(0);
    for (int ny = -1; ny <= 1; ny++) {
        for (int nx = -1; nx <= 1; nx++) {
            float2 cand = mv0.read(uint2(clamp(pc + int2(nx, ny), int2(0), chi))).xy;
            for (int oy = -1; oy <= 1; oy++) {
                for (int ox = -1; ox <= 1; ox++) {
                    float2 c = cand + float2(ox, oy);
                    int2 bd = int2(round(-c));
                    float s = P.lambda * 2.0 * length(c - parent);
                    for (int j = 0; j < B && s < best; j++)
                        for (int i = 0; i < B; i++)
                            s += abs(cb[j * B + i] - prev.read(uint2(clamp(org + int2(i, j) + bd, int2(0), hi))).r);
                    if (s < best) { best = s; bm = c; }
                }
            }
        }
    }
    // zero motion is always a candidate (static content next to a moving object)
    {
        float s = 0.0;
        for (int j = 0; j < B; j++)
            for (int i = 0; i < B; i++)
                s += abs(cb[j * B + i] - prev.read(uint2(clamp(org + int2(i, j), int2(0), hi))).r);
        if (s <= best + P.flat * 16.0) { best = s; bm = float2(0); }
    }
    mv1.write(float4(bm, saturate(1.0 - (best / 16.0) * P.confScale), 0), gid);
}

// ---- 2c. background memory -------------------------------------------------------------------------------------
// Remembers what static parts of the scene looked like, so that the area an object uncovers can be filled with the real
// background instead of a smear. alpha = how fresh the memory is.
struct BgParams { uint reset; uint pad0; uint pad1; uint pad2; };

kernel void bg_update(texture2d<float, access::read>       cur [[texture(0)]],
                      texture2d<float, access::read>       mv1 [[texture(1)]],
                      texture2d<float, access::read_write> bg  [[texture(2)]],
                      constant BgParams& P [[buffer(0)]],
                      device StatsRead* stats [[buffer(1)]],
                      uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= bg.get_width() || gid.y >= bg.get_height()) return;
    float4 c = cur.read(gid);
    // unconfirmed memory starts below the trust threshold: a pixel must be seen standing still before its memory is believed
    if (P.reset != 0 || scene_cut(stats)) { bg.write(float4(c.rgb, 0.3), gid); return; }
    float4 m = mv1.read(uint2(min(gid.x / 8, mv1.get_width() - 1), min(gid.y / 8, mv1.get_height() - 1)));
    float4 old = bg.read(gid);
    bool still = length(m.xy) < 0.35 && m.z > 0.55;
    if (still) bg.write(float4(c.rgb, 1.0), gid);
    else bg.write(float4(old.rgb, old.a * 0.96), gid);   // fresh for ~17 frames, then it expires
}

// ---- 3. motion-compensated extrapolation ---------------------------------------------------------------------
struct WarpParams { float alpha; float mvScale; float maxMv; float pad; };

// Shows `cur` advanced by `alpha` frame intervals along its measured motion. Backward warp: the output pixel takes the
// colour from where its content was `alpha` intervals ago. Low-confidence vectors fade out toward "no motion".
kernel void warp(texture2d<float, access::sample> cur [[texture(0)]],
                 texture2d<float, access::sample> mvt [[texture(1)]],
                 texture2d<float, access::write>  out [[texture(2)]],
                 constant WarpParams& P [[buffer(0)]],
                 uint2 gid [[thread_position_in_grid]]) {
    float2 size = float2(out.get_width(), out.get_height());
    if (gid.x >= out.get_width() || gid.y >= out.get_height()) return;
    constexpr sampler lin(coord::pixel, filter::linear, address::clamp_to_edge);
    constexpr sampler mvs(coord::normalized, filter::linear, address::clamp_to_edge);
    float2 p = float2(gid) + 0.5;
    float2 q = p;
    for (int it = 0; it < 2; it++) {                      // refine: sample the vector where the content really is
        float4 m = mvt.sample(mvs, q / size);
        float2 v = m.xy * P.mvScale;
        float len = length(v);
        if (len > P.maxMv) v *= P.maxMv / len;
        q = p - P.alpha * m.z * v;
    }
    out.write(cur.sample(lin, q), gid);
}

// ---- 3b. extrapolation v2: pick the right motion per pixel, find holes, fill them from memory --------------------
struct Warp2Params { float alpha; float tol; float max_err; float pad; float2 extra; float2 pad2; };

// For each output pixel, test the motion of the surrounding blocks (and "no motion") as the explanation of where its
// content came from: the source position must (a) lie in a block that moves the same way and (b) look the same in the
// previous frame once moved back. The most plausible candidate wins. If none does, the pixel is a disocclusion: show what the
// background looked like when it was last visible.
kernel void warp2(texture2d<float, access::sample> cur  [[texture(0)]],
                  texture2d<float, access::sample> prev [[texture(1)]],
                  texture2d<float, access::sample> mv1  [[texture(2)]],
                  texture2d<float, access::sample> bg   [[texture(3)]],
                  texture2d<float, access::write>  out  [[texture(4)]],
                  constant Warp2Params& P [[buffer(0)]],
                  device StatsRead* stats [[buffer(1)]],
                  uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= out.get_width() || gid.y >= out.get_height()) return;
    constexpr sampler lin(coord::pixel, filter::linear, address::clamp_to_edge);
    float2 pf = float2(gid) + 0.5;
    float2 size = float2(out.get_width(), out.get_height());
    if (scene_cut(stats)) { out.write(cur.sample(lin, pf), gid); return; }
    if (P.alpha < 0.002) { out.write(cur.sample(lin, pf + P.extra), gid); return; }
    int2 mvhi = int2(int(mv1.get_width()) - 1, int(mv1.get_height()) - 1);
    int2 fb = int2(gid) / 8;

    // 1. the pixel's own block, with its vector faded by how much we trust it (noise in flat areas must not move the picture)
    // (bilinear: the block grid must not show as steps in the picture)
    constexpr sampler mvs(coord::normalized, filter::linear, address::clamp_to_edge);
    float4 own = mv1.sample(mvs, pf / size);
    float trust = saturate((own.z - 0.05) / 0.6);
    float2 v0 = own.xy * 2.0 * trust;
    float2 q0 = pf - P.alpha * v0;
    // a source position beyond the frame edge is not an error: the picture just continues (the sampler replicates the edge)
    bool inside0 = true;
    float e0 = 1e9;
    bool ok0 = false;
    if (inside0) {
        float2 mq = mv1.read(uint2(clamp(int2(q0) / 8, int2(0), mvhi))).xy * 2.0;
        float4 oq = mv1.read(uint2(clamp(int2(q0) / 8, int2(0), mvhi)));
        float2 vq = mq * smoothstep(0.15, 0.7, oq.z);
        ok0 = length(vq - v0) <= P.tol;                                     // the block there moves the way we assumed
        float4 c = cur.sample(lin, q0);
        float4 pr = prev.sample(lin, q0 - v0);
        e0 = dot(abs(c.rgb - pr.rgb), float3(1.0 / 3.0));
        // photo-consistent is enough: noise in the vectors must not be mistaken for a hole. Only a clear visible mismatch
        // (large error AND a neighbour that moves differently) sends the pixel on to the harder cases below.
        if (e0 < P.max_err * 0.45 || (ok0 && e0 < P.max_err)) { out.write(float4(c.rgb, 1.0), gid); return; }
    }

    // 2. otherwise look at the neighbouring blocks' motion (the pixel may belong to the object next door), but only switch
    //    for a clearly better explanation
    float bestE = 1e9;
    float4 bestC = float4(0);
    for (int k = 0; k < 10; k++) {
        if (k == 4) continue;
        float2 v = float2(0);
        if (k < 9) {
            float4 m = mv1.read(uint2(clamp(fb + int2(k % 3 - 1, k / 3 - 1), int2(0), mvhi)));
            v = m.xy * 2.0 * smoothstep(0.15, 0.7, m.z);
        }
        float2 q = pf - P.alpha * v;
        if (q.x < 0.0 || q.y < 0.0 || q.x >= size.x || q.y >= size.y) continue;
        float4 mqf = mv1.read(uint2(clamp(int2(q) / 8, int2(0), mvhi)));
        float2 vq = mqf.xy * 2.0 * smoothstep(0.15, 0.7, mqf.z);
        if (length(vq - v) > P.tol) continue;
        float4 c = cur.sample(lin, q);
        float4 pr = prev.sample(lin, q - v);
        float e = dot(abs(c.rgb - pr.rgb), float3(1.0 / 3.0));
        if (e < bestE) { bestE = e; bestC = c; }
    }
    if (bestE < P.max_err * 0.5 && bestE + 0.02 < e0) { out.write(float4(bestC.rgb, 1.0), gid); return; }

    // 3. nothing explains this pixel: it was uncovered by motion. Show what the background looked like, if we remember it;
    //    otherwise fall back to the own-block guess (never to a stranger's motion)
    float4 b = bg.sample(lin, pf + P.extra);
    if (b.a > 0.5 && !ok0) { out.write(float4(b.rgb, 1.0), gid); return; }
    out.write(float4(cur.sample(lin, inside0 ? q0 + P.extra : pf + P.extra).rgb, 1.0), gid);
}

// ---- 3c. interpolation (the optional "smooth" mode; costs one source frame of latency) ------------------------------
constant float HEDGE = 0.0; // 0 = where no motion explains the pixel, show the nearer real frame (always crisp; 1 would blend the two: smoother numbers, ghosting)
struct InterpParams { float beta; float tol; float max_err; float pad; };

// The picture at fraction `beta` between the previous frame (0) and the current one (1). The content at output pixel p was at
// p - beta*v in the previous frame and will be at p + (1-beta)*v in the current one; pick the motion v whose two samples
// agree best, and blend them. Where nothing agrees (occlusion), take the nearer real frame untouched.
kernel void interp(texture2d<float, access::sample> cur  [[texture(0)]],
                   texture2d<float, access::sample> prev [[texture(1)]],
                   texture2d<float, access::read>   mv1  [[texture(2)]],
                   texture2d<float, access::write>  out  [[texture(3)]],
                   constant InterpParams& P [[buffer(0)]],
                   device StatsRead* stats [[buffer(1)]],
                   uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= out.get_width() || gid.y >= out.get_height()) return;
    constexpr sampler lin(coord::pixel, filter::linear, address::clamp_to_edge);
    float2 pf = float2(gid) + 0.5;
    float2 size = float2(out.get_width(), out.get_height());
    float4 nearest = P.beta < 0.5 ? prev.sample(lin, pf) : cur.sample(lin, pf);
    if (scene_cut(stats)) { out.write(float4(nearest.rgb, 1.0), gid); return; }
    int2 mvhi = int2(int(mv1.get_width()) - 1, int(mv1.get_height()) - 1);
    int2 fb = int2(gid) / 8;
    float bestE = 1e9;
    float4 bestC = nearest;
    for (int k = 0; k < 10; k++) {
        float2 v = float2(0);
        if (k < 9) v = mv1.read(uint2(clamp(fb + int2(k % 3 - 1, k / 3 - 1), int2(0), mvhi))).xy * 2.0;
        float2 qc = pf + (1.0 - P.beta) * v;
        float2 qp = pf - P.beta * v;
        if (qc.x < 0.0 || qc.y < 0.0 || qc.x >= size.x || qc.y >= size.y) continue;
        if (qp.x < 0.0 || qp.y < 0.0 || qp.x >= size.x || qp.y >= size.y) continue;
        float2 mq = mv1.read(uint2(clamp(int2(qc) / 8, int2(0), mvhi))).xy * 2.0;
        if (length(mq - v) > P.tol) continue;
        float4 c = cur.sample(lin, qc);
        float4 p = prev.sample(lin, qp);
        float e = dot(abs(c.rgb - p.rgb), float3(1.0 / 3.0)) - 0.0004 * length(v);
        if (e < bestE) { bestE = e; bestC = mix(p, c, P.beta); }
    }
    // where no motion explains the pixel (occlusion), the nearer real frame is used as it is: a sharp, real picture, never a blend of two
    float4 hedge = mix(prev.sample(lin, pf), cur.sample(lin, pf), P.beta);
    out.write(float4((bestE < P.max_err ? bestC : mix(nearest, hedge, HEDGE)).rgb, 1.0), gid);
}

// ---- 3d. our motion field in MetalFX's convention ------------------------------------------------------------------
// MetalFX frame interpolation wants, per pixel of the current frame, where that content was in the previous frame, as an offset in
// pixels: prev - cur = -v (our v is the motion prev -> cur). Unreliable blocks are given no motion.
kernel void motion_to_fx(texture2d<float, access::read>  mv1 [[texture(0)]],
                         texture2d<float, access::write> out [[texture(1)]],
                         uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= out.get_width() || gid.y >= out.get_height()) return;
    int2 mvhi = int2(int(mv1.get_width()) - 1, int(mv1.get_height()) - 1);
    float4 m = mv1.read(uint2(min(int2(gid) / 8, mvhi)));
    float2 v = -m.xy * 2.0 * smoothstep(0.15, 0.5, m.z);
    out.write(float4(v, 0.0, 0.0), gid);
}

// ---- 3e. sharpness of the upscaler: blended in the present pass (see `present_fs`) ---------------------------------------------------
// MetalFX's spatial scaler adds edge contrast (measured: 119% of the real picture's fine detail). The sharpness setting blends it with a plain
// smooth enlargement: 1 = the scaler as it is, 0 = smooth, in between = natural.

// ---- 4. letterboxed present ----------------------------------------------------------------------------------
struct VOut { float4 pos [[position]]; float2 uv; };

vertex VOut present_vs(uint vid [[vertex_id]]) {
    float2 p = float2((vid << 1) & 2, vid & 2);
    VOut o;
    o.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = float2(p.x, 1.0 - p.y);
    return o;
}

struct PresentParams { float2 viewport; float saturation; float sharpen; float blend; float use_blend; };

// One bilinear tap is right when the picture is shown at its own size or larger. When it is shown smaller than the stream (a small
// window) a single tap skips most of the source pixels, so fine detail shimmers and everything looks harsh: average the whole footprint
// of each output pixel instead (up to 4x4 bilinear taps, which is an area filter).
fragment float4 present_fs(VOut in [[stage_in]], texture2d<float> t [[texture(0)]], texture2d<float> plain [[texture(1)]], constant PresentParams& P [[buffer(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float2 ratio = float2(t.get_width(), t.get_height()) / max(P.viewport, float2(1.0));
    float2 texel = 1.0 / float2(t.get_width(), t.get_height());
    float3 c;
    if (max(ratio.x, ratio.y) <= 1.25) {
        c = t.sample(s, in.uv).rgb;
    } else {
        int nx = int(clamp(ceil(ratio.x), 1.0, 4.0)), ny = int(clamp(ceil(ratio.y), 1.0, 4.0));
        float3 sum = float3(0.0);
        for (int j = 0; j < ny; j++) {
            for (int i = 0; i < nx; i++) {
                float2 o = (float2((float(i) + 0.5) / float(nx), (float(j) + 0.5) / float(ny)) - 0.5) * ratio * texel;
                sum += t.sample(s, in.uv + o).rgb;
            }
        }
        c = sum / float(nx * ny);
    }
    // the upscaler's contrast against a plain smooth enlargement (see `blend_up`): done here, in the pass that is made anyway, instead of
    // in a pass of its own over the whole upscaled picture
    if (P.use_blend > 0.5) {
        float3 a = plain.sample(s, in.uv).rgb;
        c = clamp(mix(a, c, P.blend), 0.0, 1.0);   // a blend above 1 pushes past the scaler's own contrast
    }
    // user sharpness when the picture was not upscaled: unsharp mask (negative = soften)
    if (abs(P.sharpen) > 0.01) {
        float2 o = max(ratio, float2(1.0)) * texel;
        float3 blur = 0.25 * (t.sample(s, in.uv + float2(o.x, 0.0)).rgb + t.sample(s, in.uv - float2(o.x, 0.0)).rgb
                            + t.sample(s, in.uv + float2(0.0, o.y)).rgb + t.sample(s, in.uv - float2(0.0, o.y)).rgb);
        c = c + P.sharpen * (c - blur);
    }
    // saturation: scale the colour's distance from its own luma
    float l = dot(c, float3(0.2126, 0.7152, 0.0722));
    c = mix(float3(l), c, P.saturation);
    return float4(clamp(c, 0.0, 1.0), 1.0);
}
"#;
