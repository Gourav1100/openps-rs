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
    // stored as the square root: the motion search compares dark pictures (this is often what a game is) with more steps where the steps
    // of 8 bits are coarsest, so noise of a level or two does not decide what counts as flat
    dst.write(float4(sqrt(dot(c, LUMA)), 0, 0, 1), gid);
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
    float a = cur.read(p).r, b = prev.read(p).r;   // (the luma is stored as a square root)
    if (abs(a * a - b * b) > 0.015) atomic_fetch_add_explicit(out, 1, memory_order_relaxed);
}

// ---- 1c. the picture as a whole: a quarter-size luma, and the one motion that explains most of it ----------------------------------------
kernel void down4(texture2d<float, access::read>  src [[texture(0)]],
                  texture2d<float, access::write> dst [[texture(1)]],
                  uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= dst.get_width() || gid.y >= dst.get_height()) return;
    int2 hi = int2(int(src.get_width()) - 1, int(src.get_height()) - 1);
    float s = 0.0;
    for (int j = 0; j < 4; j++)
        for (int i = 0; i < 4; i++)
            s += src.read(uint2(min(int2(gid) * 4 + int2(i, j), hi))).r;
    dst.write(float4(s * (1.0 / 16.0), 0, 0, 1), gid);
}

constant int GLOBAL_R = 12;   // quarter-size pixels: 48 half-size pixels, 96 pixels of the picture, a frame interval
// One thread group, one thread per candidate shift: how well does the whole previous picture, moved by that shift, match the current one.
// Writes the motion prev -> cur in half-size pixels, and in z whether it is a clear win over no motion at all (1) or not (0).
kernel void global_search(texture2d<float, access::read> cur  [[texture(0)]],
                          texture2d<float, access::read> prev [[texture(1)]],
                          device float4* out [[buffer(0)]],
                          uint2 t [[thread_position_in_threadgroup]]) {
    threadgroup float score[(2 * GLOBAL_R + 1) * (2 * GLOBAL_R + 1)];
    int n = 2 * GLOBAL_R + 1;
    int2 d = int2(t) - GLOBAL_R;
    int2 size = int2(cur.get_width(), cur.get_height());
    float sum = 0.0, cnt = 0.0;
    for (int y = 0; y < size.y; y += 2) {
        for (int x = 0; x < size.x; x += 2) {
            int2 q = int2(x, y) + d;
            if (q.x < 0 || q.y < 0 || q.x >= size.x || q.y >= size.y) continue;
            sum += abs(cur.read(uint2(x, y)).r - prev.read(uint2(q)).r);
            cnt += 1.0;
        }
    }
    float total = float(((size.x + 1) / 2) * ((size.y + 1) / 2));
    // a shift that leaves too little of the picture to compare cannot win
    score[t.y * n + t.x] = cnt > total * 0.5 ? sum / cnt : 1e9;
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (t.x == 0 && t.y == 0) {
        float best = 1e9;
        int bi = GLOBAL_R * n + GLOBAL_R;
        for (int i = 0; i < n * n; i++) {
            float sc = score[i];
            // the shorter shift among near-equal scores
            int2 dd = int2(i % n, i / n) - GLOBAL_R;
            sc += 0.0005 * float(abs(dd.x) + abs(dd.y));
            if (sc < best) { best = sc; bi = i; }
        }
        float zero = score[GLOBAL_R * n + GLOBAL_R];
        int2 bd = int2(bi % n, bi / n) - GLOBAL_R;
        // the best score anywhere else (not next to the winner): a picture made of repeating lines matches itself at every multiple of the
        // spacing, so a winner that is not clearly better than all of those is a guess and is not used
        float other = 1e9;
        for (int i = 0; i < n * n; i++) {
            int2 dd = int2(i % n, i / n) - GLOBAL_R;
            if (max(abs(dd.x - bd.x), abs(dd.y - bd.y)) > 2) other = min(other, score[i]);
        }
        bool clear = (bd.x != 0 || bd.y != 0) && score[bi] < zero * 0.6 && zero > 0.004 && score[bi] < other * 0.75;
        out[0] = float4(clear ? float2(-bd) * 4.0 : float2(0), clear ? 1.0 : 0.0, 0);
    }
}

// ---- 2. block motion search ----------------------------------------------------------------------------------
struct MEParams { int radius; float lambda; float flat; float confScale; float lambdaRefine; float temporal; float p1; float p2; };
// How much better the winner must be than a look-alike for the block to be clear: a third of its own mismatch plus a little (the scenes
// are often dark and flat, where all the mismatches are small, so a fixed margin would call every block ambiguous)
inline float ambig(float best) { return 0.35 * best + 1.5; }
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
                   device const float4* G [[buffer(2)]],
                   constant float4& Ip [[buffer(3)]],
                   texture2d<float, access::read>  mvPrev [[texture(3)]],
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
    if (zero < P.flat * 64.0) { mv.write(float4(0, 0, 1, 0), gid); return; }   // (w = 0: no information about the vector)

    // what the controller says the camera is doing: the motion the stick has commanded between these two frames, from what was learned of
    // the game's response (x, y in half-size pixels as the vectors are, z how far to trust it; z = 0: nothing learned yet). The stick knows
    // a fast pan, and which line of a repeating pattern is the right one, before the picture does.
    int2 ibd = int2(round(-Ip.xy));
    bool useI = Ip.z > 0.0 && (ibd.x != 0 || ibd.y != 0);
    float best = zero;
    float bestNear = 0.0;   // the part of `best` that is the cost of the vector's length, not a mismatch
    int2 bd = int2(0);
    // the best match far from the winner (a look-alike: another line of a repeating pattern, a repeated texture): if it is nearly as good
    // the block is ambiguous, which `propagate` settles from the neighbours that are not
    float second = best + ambig(best);
    // Two windows: around no motion, and around the motion of the picture as a whole (the camera: `global_search`), so a fast pan is found
    // far outside the small window. Among near-equal matches the vector nearest to either centre wins: a row of identical lines matches
    // at every multiple of their spacing, and only the one that agrees with the rest of the picture is the real one.
    float4 g = G[0];
    int2 gbd = int2(round(-g.xy));
    bool useG = g.z > 0.5 && (gbd.x != 0 || gbd.y != 0);
    // what this block did a frame ago: a camera does not jump from one multiple of the line spacing to the next between two frames, so the
    // vector nearest to the last one is the real one, and the picture does not shimmer from frame to frame
    int2 tbd = int2(0);
    bool useT = false;
    if (P.temporal > 0.5) {
        float4 t = mvPrev.read(gid);
        tbd = int2(round(-t.xy));
        useT = t.z > 0.5 && (tbd.x != 0 || tbd.y != 0) && (abs(tbd.x) + abs(tbd.y)) > 0;
    }
    int2 ctrs[4] = { int2(0), gbd, tbd, ibd };
    bool use[4] = { true, useG, useT, useI };
    for (int w = 0; w < 4; w++) {
        if (!use[w]) continue;
        int2 ctr = ctrs[w];
        for (int oy = -P.radius; oy <= P.radius; oy++) {
            for (int ox = -P.radius; ox <= P.radius; ox++) {
                int2 d = ctr + int2(ox, oy);
                if (d.x == 0 && d.y == 0) continue;
                // a vector that an earlier window already tried
                bool seen = false;
                for (int e = 0; e < w; e++)
                    if (use[e] && abs(d.x - ctrs[e].x) <= P.radius && abs(d.y - ctrs[e].y) <= P.radius) seen = true;
                if (seen) continue;
                float near = float(abs(d.x) + abs(d.y));
                if (useG) near = min(near, float(abs(d.x - gbd.x) + abs(d.y - gbd.y)) + 3.0);   // the whole-picture estimate is coarse: trusted less than last frame's vector
                if (useT) near = min(near, float(abs(d.x - tbd.x) + abs(d.y - tbd.y)));
                if (useI) near = min(near, float(abs(d.x - ibd.x) + abs(d.y - ibd.y)) + (1.0 - Ip.z) * 6.0);
                float s = P.lambda * near;
                for (int j = 0; j < B && s < second; j++)
                    for (int i = 0; i < B; i++)
                        s += abs(cb[j * B + i] - prev.read(uint2(clamp(org + int2(i, j) + d, int2(0), hi))).r);
                bool far = max(abs(d.x - bd.x), abs(d.y - bd.y)) > 2;
                if (s < best) {
                    if (far) second = min(second, best);
                    best = s;
                    bestNear = P.lambda * near;
                    bd = d;
                } else if (s < second && far) {
                    second = s;
                }
                second = min(second, best + ambig(best));
            }
        }
    }
    // how well the block really matches (what the vector costs for its length is not a mismatch): a scene cut is when most blocks match badly
    float mismatch = max(best - bestNear, 0.0);
    float conf = saturate(1.0 - (mismatch / 64.0) * P.confScale);
    if (mismatch / 64.0 > 0.06) atomic_fetch_add_explicit(&stats->bad, 1, memory_order_relaxed);
    mv.write(float4(float(-bd.x), float(-bd.y), conf, saturate((second - best) / ambig(best))), gid);
}

// ---- 2a. ambiguous blocks take the vector of the unambiguous ones next to them ------------------------------------------------
// A block inside a pattern that repeats (the fins of a radiator) matches at every multiple of the spacing, and where the pattern has no
// other feature the wrong multiple can win over a whole patch of blocks, which then show the lines in the wrong place. The unambiguous
// blocks around the patch (the edge of the pattern, the gap above it) know the right vector: an ambiguous block adopts the neighbour's
// vector that fits it best, if that fits it nearly as well as its own, and passes on a little less certainty so that it spreads inwards.
kernel void propagate(texture2d<float, access::read>  cur  [[texture(0)]],
                      texture2d<float, access::read>  prev [[texture(1)]],
                      texture2d<float, access::read>  src  [[texture(2)]],
                      texture2d<float, access::write> dst  [[texture(3)]],
                      uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= dst.get_width() || gid.y >= dst.get_height()) return;
    float4 own = src.read(gid);
    if (own.w >= 0.5) { dst.write(own, gid); return; }
    const int B = 8;
    int2 org = int2(gid) * B;
    int2 hi = int2(int(cur.get_width()) - 1, int(cur.get_height()) - 1);
    int2 chi = int2(int(src.get_width()) - 1, int(src.get_height()) - 1);
    float cb[64];
    for (int j = 0; j < B; j++)
        for (int i = 0; i < B; i++)
            cb[j * B + i] = cur.read(uint2(clamp(org + int2(i, j), int2(0), hi))).r;
    float ownSad = 0.0;
    int2 od = int2(round(-own.xy));
    for (int j = 0; j < B; j++)
        for (int i = 0; i < B; i++)
            ownSad += abs(cb[j * B + i] - prev.read(uint2(clamp(org + int2(i, j) + od, int2(0), hi))).r);
    float4 chosen = own;
    float bestSad = ownSad + ambig(ownSad);
    for (int k = 0; k < 9; k++) {
        if (k == 4) continue;
        float4 n = src.read(uint2(clamp(int2(gid) + int2(k % 3 - 1, k / 3 - 1), int2(0), chi)));
        if (n.w < 0.5) continue;
        int2 nd = int2(round(-n.xy));
        float s = 0.0;
        for (int j = 0; j < B && s < bestSad; j++)
            for (int i = 0; i < B; i++)
                s += abs(cb[j * B + i] - prev.read(uint2(clamp(org + int2(i, j) + nd, int2(0), hi))).r);
        if (s < bestSad) { bestSad = s; chosen = float4(n.xy, n.z, n.w * 0.85); }
    }
    dst.write(chosen, gid);
}

// ---- 2a'. what the clear blocks agree on, for the blocks that nothing next to them can tell ----------------------------------------------
// A wall, a floor or the sky has no texture to match: its blocks are ambiguous and, in an expanse wider than a few blocks, have no clear
// neighbour to take a vector from. Their motion is then the picture's: the vector most of the clear blocks have (the camera), found as the
// fullest cell of a histogram of their vectors and then averaged over the blocks in it.
constant int HIST_N = 49;                  // cells of 2 half-size pixels over +-48
kernel void dominant_motion(texture2d<float, access::read> mv [[texture(0)]],
                            device float4* out [[buffer(0)]],
                            uint tid [[thread_index_in_threadgroup]]) {
    threadgroup atomic_uint hist[HIST_N * HIST_N];
    threadgroup atomic_int sums[3];
    threadgroup uint best_cell;
    for (uint i = tid; i < uint(HIST_N * HIST_N); i += 1024) atomic_store_explicit(&hist[i], 0, memory_order_relaxed);
    if (tid < 3) atomic_store_explicit(&sums[tid], 0, memory_order_relaxed);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    uint w = mv.get_width(), n = w * mv.get_height();
    uint total = 0;
    for (uint i = tid; i < n; i += 1024) {
        float4 m = mv.read(uint2(i % w, i / w));
        if (m.w < 0.5 || m.z < 0.3) continue;
        int cx = clamp(int(floor((m.x + 48.0) * 0.5)), 0, HIST_N - 1), cy = clamp(int(floor((m.y + 48.0) * 0.5)), 0, HIST_N - 1);
        atomic_fetch_add_explicit(&hist[cy * HIST_N + cx], 1, memory_order_relaxed);
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (tid == 0) {
        uint best = 0, bi = HIST_N * HIST_N / 2;
        for (uint i = 0; i < uint(HIST_N * HIST_N); i++) {
            // a cell and its eight neighbours: a motion that falls on the border of two cells still counts as one
            uint c = atomic_load_explicit(&hist[i], memory_order_relaxed);
            uint cx = i % HIST_N, cy = i / HIST_N;
            uint sum = c;
            for (int k = 0; k < 9; k++) {
                if (k == 4) continue;
                int nx = int(cx) + k % 3 - 1, ny = int(cy) + k / 3 - 1;
                if (nx >= 0 && ny >= 0 && nx < HIST_N && ny < HIST_N) sum += atomic_load_explicit(&hist[ny * HIST_N + nx], memory_order_relaxed);
            }
            if (sum > best) { best = sum; bi = i; }
        }
        best_cell = bi;
        atomic_store_explicit(&sums[2], int(best), memory_order_relaxed);
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    int bx = int(best_cell % HIST_N), by = int(best_cell / HIST_N);
    for (uint i = tid; i < n; i += 1024) {
        float4 m = mv.read(uint2(i % w, i / w));
        if (m.w < 0.5 || m.z < 0.3) continue;
        int cx = clamp(int(floor((m.x + 48.0) * 0.5)), 0, HIST_N - 1), cy = clamp(int(floor((m.y + 48.0) * 0.5)), 0, HIST_N - 1);
        if (abs(cx - bx) <= 1 && abs(cy - by) <= 1) {
            atomic_fetch_add_explicit(&sums[0], int(round(m.x * 16.0)), memory_order_relaxed);
            atomic_fetch_add_explicit(&sums[1], int(round(m.y * 16.0)), memory_order_relaxed);
            total++;
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (tid == 0) {
        int cnt = atomic_load_explicit(&sums[2], memory_order_relaxed);
        float k = 1.0 / (16.0 * max(cnt, 1));
        out[0] = float4(atomic_load_explicit(&sums[0], memory_order_relaxed) * k, atomic_load_explicit(&sums[1], memory_order_relaxed) * k,
                        float(cnt) / float(max(n, 1u)), float(cnt));
    }
}

// Blocks that are still ambiguous take that vector if their own picture fits it nearly as well as it fits its own.
kernel void adopt_dominant(texture2d<float, access::read>  cur  [[texture(0)]],
                           texture2d<float, access::read>  prev [[texture(1)]],
                           texture2d<float, access::read>  src  [[texture(2)]],
                           texture2d<float, access::write> dst  [[texture(3)]],
                           device const float4* dom [[buffer(0)]],
                           constant float4& Ip [[buffer(1)]],
                           uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= dst.get_width() || gid.y >= dst.get_height()) return;
    float4 own = src.read(gid);
    float4 d = dom[0];
    // with no consensus among the clear blocks (a picture that is all repeating pattern or all flat), the stick's motion stands in for it
    if (d.z < 0.08 && Ip.z > 0.0) d = float4(Ip.xy, 0.08, 0);
    if (own.w >= 0.5 || d.z < 0.08) { dst.write(own, gid); return; }
    const int B = 8;
    int2 org = int2(gid) * B;
    int2 hi = int2(int(cur.get_width()) - 1, int(cur.get_height()) - 1);
    int2 od = int2(round(-own.xy)), dd = int2(round(-d.xy));
    float so = 0.0, sd = 0.0;
    for (int j = 0; j < B; j++)
        for (int i = 0; i < B; i++) {
            float c = cur.read(uint2(clamp(org + int2(i, j), int2(0), hi))).r;
            so += abs(c - prev.read(uint2(clamp(org + int2(i, j) + od, int2(0), hi))).r);
            sd += abs(c - prev.read(uint2(clamp(org + int2(i, j) + dd, int2(0), hi))).r);
        }
    if (sd <= so + ambig(so)) dst.write(float4(float(-dd.x), float(-dd.y), own.z, 0.5), gid);
    else dst.write(own, gid);
}

// ---- 2b. refinement: 4x4 blocks, candidates from the neighbouring coarse blocks ------------------------------
// Coarse blocks straddle object boundaries; trying the neighbours' vectors (plus a one-pixel search around each) lets each
// small block pick the motion of the object it actually belongs to.
kernel void refine(texture2d<float, access::sample> cur  [[texture(0)]],
                   texture2d<float, access::sample> prev [[texture(1)]],
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
    // what the neighbourhood agrees on: the middle value of the nine coarse vectors around this block (a vector median). A block whose own
    // vector is a look-alike match (one line of a repeating pattern over) is pulled back to it unless its picture clearly prefers its own.
    float2 parent;
    {
        float vx[9], vy[9];
        for (int k = 0; k < 9; k++) {
            float2 v = mv0.read(uint2(clamp(pc + int2(k % 3 - 1, k / 3 - 1), int2(0), chi))).xy;
            vx[k] = v.x;
            vy[k] = v.y;
        }
        for (int a = 1; a < 9; a++) {
            float tx = vx[a], ty = vy[a];
            int b = a - 1;
            while (b >= 0 && vx[b] > tx) { vx[b + 1] = vx[b]; b--; }
            vx[b + 1] = tx;
            b = a - 1;
            while (b >= 0 && vy[b] > ty) { vy[b + 1] = vy[b]; b--; }
            vy[b + 1] = ty;
        }
        parent = float2(vx[4], vy[4]);
    }
    float best = 1e9;
    float2 bm = float2(0);
    for (int ny = -1; ny <= 1; ny++) {
        for (int nx = -1; nx <= 1; nx++) {
            float2 cand = mv0.read(uint2(clamp(pc + int2(nx, ny), int2(0), chi))).xy;
            for (int oy = -1; oy <= 1; oy++) {
                for (int ox = -1; ox <= 1; ox++) {
                    float2 c = cand + float2(ox, oy);
                    int2 bd = int2(round(-c));
                    float s = P.lambdaRefine * 2.0 * length(c - parent);
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
    // sub-pixel: the search above moves in whole half-resolution pixels (2 pixels of the picture), which leaves lines that move by an odd
    // number of pixels half a step off; the two frames of a generated picture then disagree by up to a pixel at every edge and a row of
    // thin lines (a radiator, railings) shimmers. Half and then quarter steps, from the bilinearly sampled previous frame.
    if (any(bm != float2(0))) {
        constexpr sampler lin(coord::pixel, filter::linear, address::clamp_to_edge);
        float2 centre = bm;
        float cs = 1e9;
        {   // the score of the best integer vector, on the same footing as the fractional ones
            float s = P.lambdaRefine * 2.0 * length(centre - parent);
            for (int j = 0; j < B; j++)
                for (int i = 0; i < B; i++)
                    s += abs(cb[j * B + i] - prev.sample(lin, float2(org + int2(i, j)) + 0.5 - centre).r);
            cs = s;
        }
        for (int stage = 0; stage < 2; stage++) {
            float step = stage == 0 ? 0.5 : 0.25;
            float2 base = centre;
            for (int oy = -1; oy <= 1; oy++) {
                for (int ox = -1; ox <= 1; ox++) {
                    if (ox == 0 && oy == 0) continue;
                    float2 c = base + float2(ox, oy) * step;
                    float s = P.lambdaRefine * 2.0 * length(c - parent);
                    for (int j = 0; j < B && s < cs; j++)
                        for (int i = 0; i < B; i++)
                            s += abs(cb[j * B + i] - prev.sample(lin, float2(org + int2(i, j)) + 0.5 - c).r);
                    if (s < cs) { cs = s; centre = c; }
                }
            }
        }
        bm = centre;
        best = min(best, cs);
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
constant float HEDGE = 0.75; // where no motion explains the pixel (an occlusion, a vector that is wrong here): 0 shows the nearer real frame (crisp, but its edges are where the camera was, not where it is: they jump), 1 blends the two (a soft ghost, steady from frame to frame); the replay of a real session errs least at 1
struct InterpParams { float beta; float tol; float max_err; float pad; };

// Catmull-Rom (bicubic) sample at `pos` (pixel coordinates, texel centres at +0.5). A plain bilinear sample at a fractional position
// softens one-pixel lines, and a generated frame between two sharp real ones that is softer than they are makes thin lines flicker.
inline float4 sample_sharp(texture2d<float, access::sample> t, float2 pos) {
    float2 p = pos - 0.5;
    float2 f = floor(p);
    float2 u = p - f;
    // Keys' cubic with the sharpness `A` (-0.5 is Catmull-Rom; -0.75 keeps more of the fine detail of a picture moved by a fraction of a pixel,
    // at the price of a little overshoot, which the saturate below cuts)
    constexpr float A = -0.75;
    float2 u2 = u * u, u3 = u2 * u;
    float2 w0 = A * u3 - 2.0 * A * u2 + A * u;
    float2 w1 = (A + 2.0) * u3 - (A + 3.0) * u2 + 1.0;
    float2 v = 1.0 - u, v2 = v * v, v3 = v2 * v;
    float2 w2 = (A + 2.0) * v3 - (A + 3.0) * v2 + 1.0;
    float2 w3 = A * v3 - 2.0 * A * v2 + A * v;
    int2 hi = int2(int(t.get_width()) - 1, int(t.get_height()) - 1);
    int2 b = int2(f);
    float4 acc = float4(0);
    float wy[4] = { w0.y, w1.y, w2.y, w3.y };
    float wx[4] = { w0.x, w1.x, w2.x, w3.x };
    for (int j = 0; j < 4; j++) {
        float4 row = float4(0);
        for (int i = 0; i < 4; i++)
            row += wx[i] * t.read(uint2(clamp(b + int2(i - 1, j - 1), int2(0), hi)));
        acc += wy[j] * row;
    }
    return saturate(acc);
}

// How well the two frames agree around a pair of positions: the mean colour difference over a 3x3 window.
constant float OWN_FITS = 0.05;
inline float window_error(texture2d<float, access::sample> cur, texture2d<float, access::sample> prev, float2 qc, float2 qp) {
    constexpr sampler lin(coord::pixel, filter::linear, address::clamp_to_edge);
    float e = 0.0;
    for (int j = -1; j <= 1; j++)
        for (int i = -1; i <= 1; i++) {
            float2 o = float2(i, j);
            e += dot(abs(cur.sample(lin, qc + o).rgb - prev.sample(lin, qp + o).rgb), float3(1.0 / 3.0));
        }
    return e / 9.0;
}

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
    float2 bestQc = pf, bestQp = pf;
    // The block's own vector first. When its two samples agree it is used as it is. Only when they do not (an occlusion, or a vector that
    // is wrong here) are the neighbours' vectors tried, and then by how well a small window around the pixel agrees, not the pixel alone:
    // among ten vectors a single pixel always finds one that fits by luck, above all in a pattern that repeats.
    {
        float2 v = mv1.read(uint2(clamp(fb, int2(0), mvhi))).xy * 2.0;
        float2 qc = pf + (1.0 - P.beta) * v;
        float2 qp = pf - P.beta * v;
        bool inside = qc.x >= 0.0 && qc.y >= 0.0 && qc.x < size.x && qc.y < size.y && qp.x >= 0.0 && qp.y >= 0.0 && qp.x < size.x && qp.y < size.y;
        if (inside) {
            float e = window_error(cur, prev, qc, qp);
            if (e < OWN_FITS) { bestE = e; bestQc = qc; bestQp = qp; }
        }
    }
    for (int k = 0; k < 10 && bestE >= OWN_FITS; k++) {
        float2 v = float2(0);
        if (k < 9) v = mv1.read(uint2(clamp(fb + int2(k % 3 - 1, k / 3 - 1), int2(0), mvhi))).xy * 2.0;
        float2 qc = pf + (1.0 - P.beta) * v;
        float2 qp = pf - P.beta * v;
        if (qc.x < 0.0 || qc.y < 0.0 || qc.x >= size.x || qc.y >= size.y) continue;
        if (qp.x < 0.0 || qp.y < 0.0 || qp.x >= size.x || qp.y >= size.y) continue;
        float2 mq = mv1.read(uint2(clamp(int2(qc) / 8, int2(0), mvhi))).xy * 2.0;
        if (length(mq - v) > P.tol) continue;
        float e = window_error(cur, prev, qc, qp) - 0.0004 * length(v) - (k == 4 ? 0.02 : 0.0);
        if (e < bestE) { bestE = e; bestQc = qc; bestQp = qp; }
    }
    // where no motion explains the pixel (occlusion), the nearer real frame is used as it is: a sharp, real picture, never a blend of two
    float4 hedge = mix(prev.sample(lin, pf), cur.sample(lin, pf), P.beta);
    // how much of the newer picture: the position between the two (`pad` below 0), a step of width `pad` around the middle, or (0) the
    // nearer one alone. Averaging two pictures loses detail and noise (the average of two independent grains is quieter than either), so a generated
    // picture would be softer than the real ones around it, and the detail would pulse at the beat of the generated frames.
    float wc = P.pad < 0.0 ? P.beta : (P.pad > 0.0 ? smoothstep(0.5 - P.pad, 0.5 + P.pad, P.beta) : step(0.5, P.beta));
    float4 made = bestE < P.max_err ? mix(sample_sharp(prev, bestQp), sample_sharp(cur, bestQc), wc) : mix(nearest, hedge, HEDGE);
    out.write(float4(made.rgb, 1.0), gid);
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

// ---- 3e. finishing the upscaler's picture -----------------------------------------------------------------------------------------------
// MetalFX's spatial scaler adds edge contrast (measured: 119% of the real picture's fine detail). The sharpness setting blends it with a plain
// smooth enlargement: 1 = the scaler as it is, 0 = smooth, in between = natural. That blend takes some crispness away from a stream that was
// well encoded, so FSR's RCAS (robust contrast adaptive sharpening, ported from AMD's `FsrRcasF`, same 5-tap cross, ring limiters and
// limit) puts part of it back, in a strength that follows the setting (see `rcas_scale`). The scaler may run in linear light (measured:
// better than its perceptual mode at every bitrate): then its half-float output is converted back here. All in one pass.
float3 srgb_to_linear(float3 c) { return select(c / 12.92, pow((c + 0.055) / 1.055, 2.4), c > 0.04045); }
float3 linear_to_srgb(float3 c) { return select(c * 12.92, 1.055 * pow(c, 1.0 / 2.4) - 0.055, c > 0.0031308); }

kernel void to_linear(texture2d<float, access::read> src [[texture(0)]], texture2d<float, access::write> dst [[texture(1)]],
                      uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= dst.get_width() || gid.y >= dst.get_height()) return;
    dst.write(float4(srgb_to_linear(src.read(gid).rgb), 1.0), gid);
}

struct FinishParams { float blend; float rcas; float linear; float pad; };   // rcas = the lobe scale (2^-stops), 0 = off

// Run in 16x16 threadgroups: the blended pixel is worked out once per pixel (plus a 1-pixel border) into threadgroup memory and RCAS reads its
// cross from there, instead of every pixel redoing the colour conversion and the blend for five taps. Dispatched as whole threadgroups: every
// thread of a group loads part of the tile, so a partial group at the picture's edge would leave part of it unloaded.
kernel void finish_up(texture2d<float, access::read> up [[texture(0)]], texture2d<float, access::sample> plain [[texture(1)]],
                      texture2d<float, access::write> dst [[texture(2)]], constant FinishParams& P [[buffer(0)]],
                      uint2 gid [[thread_position_in_grid]], uint2 tid [[thread_position_in_threadgroup]],
                      uint2 tg [[threadgroup_position_in_grid]]) {
    threadgroup float3 tile[18][18];
    int w = dst.get_width(), h = dst.get_height();
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    int2 origin = int2(tg) * 16 - 1;
    uint lin = tid.y * 16 + tid.x;
    for (uint i = lin; i < 18 * 18; i += 256) {
        int2 p = clamp(origin + int2(i % 18, i / 18), int2(0), int2(w - 1, h - 1));
        float3 f = up.read(uint2(p)).rgb;
        if (P.linear > 0.5) f = linear_to_srgb(saturate(f));
        float3 a = plain.sample(s, (float2(p) + 0.5) / float2(w, h)).rgb;
        tile[i / 18][i % 18] = saturate(mix(a, f, P.blend));   // a blend above 1 pushes past the scaler's own contrast
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (int(gid.x) >= w || int(gid.y) >= h) return;
    int tx = int(tid.x) + 1, ty = int(tid.y) + 1;
    float3 e = tile[ty][tx], o = e;
    if (P.rcas > 0.0) {
        float3 b = tile[ty - 1][tx], d = tile[ty][tx - 1], f = tile[ty][tx + 1], hh = tile[ty + 1][tx];
        float3 mn4 = min(min(min(b, d), f), hh), mx4 = max(max(max(b, d), f), hh);
        // all-black / all-white rings: the reference divides by zero there; no sharpening is what it comes to
        float3 hitMin = select(float3(0.0), min(mn4, e) / (4.0 * mx4), mx4 > 0.0);
        float3 hitMax = select(float3(0.0), (1.0 - max(mx4, e)) / (4.0 * mn4 - 4.0), mn4 < 1.0);
        float3 lobe3 = max(-hitMin, hitMax);
        float lobe = max(-0.1875, min(max(max(lobe3.r, lobe3.g), lobe3.b), 0.0)) * P.rcas;
        o = (lobe * (b + d + f + hh) + e) / (4.0 * lobe + 1.0);
    }
    dst.write(float4(saturate(o), 1.0), gid);
}

// ---- 3f. FSR 1: EASU (edge-adaptive spatial upsampling) ---------------------------------------------------------------------------------
// Ported from AMD's FidelityFX FSR 1 `FsrEasuF` (ffx_fsr1.h), MIT licence, Copyright (c) 2021 Advanced Micro Devices, Inc.:
// "Permission is hereby granted, free of charge, to any person obtaining a copy of this software ... to deal in the Software without
// restriction ... provided that the above copyright notice and this permission notice shall be included in all copies or substantial
// portions of the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND."
// Same 12-tap kernel, direction and length estimate, Lanczos-like window and dering. The taps are read one texel at a time (clamped at the
// edges, what the reference's clamped gather does) instead of with gather4; the reference's reciprocal approximations are kept as they are.
// Works on the picture as the stream has it (gamma-encoded), which is what FSR 1 is made for.
inline float fsr_rcp(float a) { return as_type<float>(0x7ef07ebbu - as_type<uint>(a)); }
inline float fsr_rsq(float a) { return as_type<float>(0x5f347d74u - (as_type<uint>(a) >> 1)); }

inline void easu_tap(thread float3& aC, thread float& aW, float2 off, float2 dir, float2 len, float lob, float clp, float3 c) {
    float2 v = float2(off.x * dir.x + off.y * dir.y, off.x * (-dir.y) + off.y * dir.x);
    v *= len;
    float d2 = min(v.x * v.x + v.y * v.y, clp);
    float wB = 2.0 / 5.0 * d2 - 1.0;
    float wA = lob * d2 - 1.0;
    wB *= wB;
    wA *= wA;
    wB = 25.0 / 16.0 * wB - (25.0 / 16.0 - 1.0);
    float w = wB * wA;
    aC += c * w;
    aW += w;
}

inline void easu_set(thread float2& dir, thread float& len, float2 pp, bool biS, bool biT, bool biU, bool biV,
                     float lA, float lB, float lC, float lD, float lE) {
    float w = 0.0;
    if (biS) w = (1.0 - pp.x) * (1.0 - pp.y);
    if (biT) w = pp.x * (1.0 - pp.y);
    if (biU) w = (1.0 - pp.x) * pp.y;
    if (biV) w = pp.x * pp.y;
    float dc = lD - lC, cb = lC - lB;
    float lenX = fsr_rcp(max(abs(dc), abs(cb)));
    float dirX = lD - lB;
    dir.x += dirX * w;
    lenX = saturate(abs(dirX) * lenX);
    lenX *= lenX;
    len += lenX * w;
    float ec = lE - lC, ca = lC - lA;
    float lenY = fsr_rcp(max(abs(ec), abs(ca)));
    float dirY = lE - lA;
    dir.y += dirY * w;
    lenY = saturate(abs(dirY) * lenY);
    lenY *= lenY;
    len += lenY * w;
}

struct EasuParams { float2 scale; float2 pad; };   // the input size over the output size

kernel void fsr_easu(texture2d<float, access::read> src [[texture(0)]], texture2d<float, access::write> dst [[texture(1)]],
                     constant EasuParams& P [[buffer(0)]], uint2 ip [[thread_position_in_grid]]) {
    if (ip.x >= dst.get_width() || ip.y >= dst.get_height()) return;
    int2 hi = int2(src.get_width() - 1, src.get_height() - 1);
    float2 pp = (float2(ip) + 0.5) * P.scale - 0.5;
    float2 fp = floor(pp);
    pp -= fp;
    int2 f0 = int2(fp);
    // the 12 taps:   b c / e f g h / i j k l / n o
    auto at = [&](int dx, int dy) { return src.read(uint2(clamp(f0 + int2(dx, dy), int2(0), hi))).rgb; };
    float3 cb = at(0, -1), cc = at(1, -1);
    float3 ce = at(-1, 0), cf = at(0, 0), cg = at(1, 0), ch = at(2, 0);
    float3 ci = at(-1, 1), cj = at(0, 1), ck = at(1, 1), cl = at(2, 1);
    float3 cn = at(0, 2), co = at(1, 2);
    // the simplest luma (times 2)
    auto luma = [](float3 c) { return c.b * 0.5 + (c.r * 0.5 + c.g); };
    float bL = luma(cb), cL = luma(cc), eL = luma(ce), fL = luma(cf), gL = luma(cg), hL = luma(ch);
    float iL = luma(ci), jL = luma(cj), kL = luma(ck), lL = luma(cl), nL = luma(cn), oL = luma(co);
    float2 dir = float2(0.0);
    float len = 0.0;
    easu_set(dir, len, pp, true, false, false, false, bL, eL, fL, gL, jL);
    easu_set(dir, len, pp, false, true, false, false, cL, fL, gL, hL, kL);
    easu_set(dir, len, pp, false, false, true, false, fL, iL, jL, kL, nL);
    easu_set(dir, len, pp, false, false, false, true, gL, jL, kL, lL, oL);
    float2 dir2 = dir * dir;
    float dirR = dir2.x + dir2.y;
    bool zro = dirR < 1.0 / 32768.0;
    dirR = fsr_rsq(dirR);
    dirR = zro ? 1.0 : dirR;
    dir.x = zro ? 1.0 : dir.x;
    dir *= dirR;
    len = len * 0.5;
    len *= len;
    float stretch = (dir.x * dir.x + dir.y * dir.y) * fsr_rcp(max(abs(dir.x), abs(dir.y)));
    float2 len2 = float2(1.0 + (stretch - 1.0) * len, 1.0 + -0.5 * len);
    float lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    float clp = fsr_rcp(lob);
    float3 min4 = min(min(min(cf, cg), cj), ck);
    float3 max4 = max(max(max(cf, cg), cj), ck);
    float3 aC = float3(0.0);
    float aW = 0.0;
    easu_tap(aC, aW, float2(0.0, -1.0) - pp, dir, len2, lob, clp, cb);
    easu_tap(aC, aW, float2(1.0, -1.0) - pp, dir, len2, lob, clp, cc);
    easu_tap(aC, aW, float2(-1.0, 1.0) - pp, dir, len2, lob, clp, ci);
    easu_tap(aC, aW, float2(0.0, 1.0) - pp, dir, len2, lob, clp, cj);
    easu_tap(aC, aW, float2(0.0, 0.0) - pp, dir, len2, lob, clp, cf);
    easu_tap(aC, aW, float2(-1.0, 0.0) - pp, dir, len2, lob, clp, ce);
    easu_tap(aC, aW, float2(1.0, 1.0) - pp, dir, len2, lob, clp, ck);
    easu_tap(aC, aW, float2(2.0, 1.0) - pp, dir, len2, lob, clp, cl);
    easu_tap(aC, aW, float2(2.0, 0.0) - pp, dir, len2, lob, clp, ch);
    easu_tap(aC, aW, float2(1.0, 0.0) - pp, dir, len2, lob, clp, cg);
    easu_tap(aC, aW, float2(1.0, 2.0) - pp, dir, len2, lob, clp, co);
    easu_tap(aC, aW, float2(0.0, 2.0) - pp, dir, len2, lob, clp, cn);
    float3 pix = min(max4, max(min4, aC * (1.0 / aW)));
    dst.write(float4(pix, 1.0), ip);
}

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

// ---- 5. immersion: the glow behind the picture ---------------------------------------------------------------------------------
// The whole picture, shrunk to a tiny image (`glow_down_fs`) and stretched over the whole window (so a wide window stretches it
// sideways, a tall one upwards), then blurred again (`ambient_fs`). One soft wash of the picture's colours, like the ambient light of
// a video site, with no copy of the picture, no mirroring and no hard detail in it. Drawn only into the bars, before the picture.
fragment float4 glow_down_fs(VOut in [[stage_in]], texture2d<float> t [[texture(0)]], constant float2& cell [[buffer(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float3 sum = float3(0.0);
    for (int j = 0; j < 12; j++) {                              // the average of the part of the picture this pixel stands for
        for (int i = 0; i < 12; i++) {
            float2 o = (float2((float(i) + 0.5) / 12.0, (float(j) + 0.5) / 12.0) - 0.5) * cell;
            sum += t.sample(s, in.uv + o).rgb;
        }
    }
    return float4(sum / 144.0, 1.0);
}

struct AmbientParams { float2 target; float dim; float radius; };   // the window's size in pixels; brightness; blur radius in window widths

fragment float4 ambient_fs(VOut in [[stage_in]], texture2d<float> g [[texture(0)]], constant AmbientParams& A [[buffer(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float2 uv = in.pos.xy / A.target;                           // the glow image stretched over the window
    float3 sum = float3(0.0);
    for (int i = 0; i < 24; i++) {                              // a spiral of taps (a Vogel disc) over the tiny image: smooth, no steps
        float a = float(i) * 2.39996323;
        float r = sqrt((float(i) + 0.5) / 24.0) * A.radius;
        sum += g.sample(s, uv + float2(cos(a), sin(a)) * r).rgb;
    }
    float3 c = sum / 24.0 * A.dim;
    float n = fract(sin(dot(in.pos.xy, float2(12.9898, 78.233))) * 43758.5453) - 0.5;   // a hair of noise so the soft ramps do not band
    return float4(c + n / 255.0, 1.0);
}

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
