// Retro starfield: stars streaming outwards from a vanishing point, each with
// a short phosphor streak.
//
// Perspective, not a zoom: a star sits at a fixed offset from the flight axis
// and its depth falls steadily, so its screen radius is `offset / depth` and it
// accelerates outwards as it gets close. The offset is randomised per angular
// column (and per band), which is what gives the field depth — at the same
// point on screen, a near star is fast, fat and bright while a far one is a
// slow pinprick — without the stars falling onto a few discrete cylinders.
//
// Rather than loop over a list of stars per pixel (which caps how many are
// affordable), each band is a lattice in (angle, depth): one star per cell,
// jittered within it, and the whole lattice slides as depth falls. A pixel maps
// its own radius straight back to a depth, so it only has to look at the handful
// of cells around its own, whatever the star count. Nothing spawns or wraps.
// `u.seed` reshuffles the lattice per monitor.
//
// Cells of constant depth project to a density that goes as 1/radius^3, which
// piles the whole field into a knot at the centre, so each star also gets a
// threshold it fades in at, thinning the field by radius^3 to leave the even
// scattering the real thing has. That fade is stretched over most of the screen
// — a star takes several seconds to come up — so it reads as something far off
// growing closer rather than as a star popping into existence.

struct Uniforms {
    time: f32,
    seed: f32,
    resolution: vec2<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var verts = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(verts[vi], 0.0, 1.0);
}

// Cheap deterministic hash (Hoskins) → two values in [0,1).
fn hash22(p: vec2<f32>) -> vec2<f32> {
    var q = fract(vec3<f32>(p.xyx) * vec3<f32>(0.1031, 0.1030, 0.0973));
    q += dot(q, q.yzx + 33.33);
    return fract((q.xx + q.yz) * q.zy);
}

// Distance from p to the segment a--b, plus the clamped projection parameter
// (0 at a, 1 at b).
fn seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / (dot(ba, ba) + 1e-9), 0.0, 1.0);
    return vec2<f32>(length(pa - ba * h), h);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let res = u.resolution;
    // Centred and aspect-corrected: vanishing point at (0,0), y in [-0.5,0.5].
    let p = (frag.xy - 0.5 * res) / res.y;
    let tau = 6.28318;

    // Depth is measured in multiples of the star's own offset from the axis, so
    // its screen radius is just 1/depth and the lattice is the same shape for
    // every offset. The offset then only sets how fast the star crosses that
    // depth: close ones tear past, distant ones crawl.
    let bands = 3;      // offsets drawn per column, spread over the range below
    let nearest = 0.045;    // smallest offset from the flight axis
    let span = 1.5;         // ...and the log range above it (e^1.5 ≈ 4.5x)
    let flight = 0.07;      // camera speed, in offset units per second
    // How much of each band actually gets drawn. The near band is the fast,
    // fat, bright one, so it stays rare; most of the field is the far band's
    // slow pinpricks. These are constants, so they thin the field without any
    // star ever appearing or vanishing mid-flight.
    var weight = array<f32, 3>(0.22, 0.78, 1.00);

    // Fewer, wider columns than the star count alone would want, with the
    // weights above carrying the density instead. A column's width is what
    // limits how close to the vanishing point a star can still be drawn whole
    // — narrower ones clip it sideways into a dash — so this is what keeps the
    // dark patch in the middle small.
    let cols = 160.0;   // cells around a full turn
    let dz = 0.14;      // cell height, in units of depth
    let lit = 1.05;     // radius at which every star that will show is showing.
                        // That is the far corner, so the fade-in is spread
                        // across the whole screen rather than bunched at an
                        // edge where it would be caught happening.
    let tail = 0.016;   // afterglow length, in seconds of travel. Keep the
                        // fastest band's `rate * tail` under `dz`, or a streak
                        // could reach a pixel from outside the cells it checks.
    let smear = 0.008;  // ...and the longest streak it may leave on screen
    let rows = 512.0;   // the lattice repeats after this many rows (keeps the
                        // hash inputs small however long the saver runs)

    // This pixel's angle, and the depth at which a star would land on it.
    let rad = length(p);
    let gx = (atan2(p.y, p.x) / tau + 0.5) * cols;
    let depth = 1.0 / max(rad, 1e-4);

    // Rows are `dz * radius^2` apart on screen, so towards the centre they
    // crowd in tighter than a star is wide and the neighbouring cells alone
    // stop covering one, which clips the stars there into slivers. Widen the
    // row window to suit — it only costs anything near the middle, where
    // nearly every cell is thinned away below. The 0.004 covers a whole glow,
    // not just its core: clipping even the faint skirt shows up, as a ring at
    // every radius where the window steps.
    let win = i32(clamp(ceil(0.004 / (rad * rad * dz)), 1.0, 512.0));

    var col = vec3<f32>(0.0);
    for (var b = 0; b < bands; b = b + 1) {
        for (var k = -1; k <= 1; k = k + 1) {
            let cx = floor(gx) + f32(k);
            // Wrap the column before hashing: angle is periodic. Positions
            // still use the unwrapped column.
            let cix = cx - cols * floor(cx / cols);

            // One offset per (column, band), stratified so the bands stay
            // spread out while no two columns share an offset exactly.
            let oh = hash22(vec2<f32>(cix, f32(b) * 13.0) + u.seed * vec2<f32>(5.3, 11.9));
            let off = nearest * exp(span * (f32(b) + oh.x) / f32(bands));
            let rate = flight / off;      // depth lost per second
            let slide = rate * u.time;
            let rowb = floor((depth + slide) / dz);

            for (var j = -win; j <= win; j = j + 1) {
                let cy = rowb + f32(j);
                let id = vec2<f32>(cix, cy - rows * floor(cy / rows))
                       + f32(b) * vec2<f32>(31.0, 17.0);
                // Two rounds: hashing the grid position directly leaves faint
                // diagonal structure, and the lattice shows through.
                let h = hash22(id + u.seed * vec2<f32>(13.7, 7.3));
                let jit = hash22(h * 37.0);          // position within the cell

                // The star's depth now, and `tail` ago. Since it only ever
                // moves along its own ray, the streak is exactly the segment
                // between the two — no sampling of past instants.
                let zs = (cy + jit.y) * dz - slide;
                if (zs < 0.6) {
                    continue;                        // past the camera already
                }
                let sr = 1.0 / zs;

                // Thin the field out towards the centre: cells of constant
                // depth crowd up as 1/radius^3 there, so each star gets its own
                // threshold and only fades in once radius^3 passes it, leaving
                // an even scattering. The 50:1 ramp is what keeps that fade
                // slow enough not to be seen as a star arriving.
                let fade = clamp(sr / lit, 0.0, 1.0);
                let rank = 0.00005 + h.y / weight[b];
                let show = smoothstep(rank * 0.02, rank, fade * fade * fade);
                if (show <= 0.0) {
                    continue;
                }
                let vary = hash22(h * 91.0 + 7.0);   // magnitude, tint

                // Cap how far the streak reaches: without it the fastest
                // stars smear right across the corners of the screen.
                let pr = max(1.0 / (zs + rate * tail), sr - smear);
                let sa = ((cx + jit.x) / cols - 0.5) * tau;
                let dir = vec2<f32>(cos(sa), sin(sa));
                let sd = seg(p, dir * pr, dir * sr);

                // Nearer stars are bigger and brighter; each has its own
                // magnitude. `sr / off` is the inverse of the true distance.
                let near = sr / off;
                // Magnitude has a long tail: mostly faint specks, with the
                // occasional star that reads as a fat bright one.
                let v2 = vary.x * vary.x;
                let v4 = v2 * v2;
                let mag = 0.35 + 0.65 * v2 + 1.6 * v4 * v4 * v4;
                // Magnitude only fattens a star as it closes in, so the far
                // ones stay the same small dot however bright they are. A fat
                // star near the vanishing point would be clipped by its cell.
                let w = 0.00062 + (0.00042 + 0.00052 * mag) * clamp(near / 12.0, 0.0, 1.0);
                if (sd.x > 4.0 * w) {
                    continue;
                }

                // Fade by how far behind the head the pixel is, in absolute
                // distance rather than as a fraction of the streak: within a
                // star's width of the head nothing fades, so the star stays a
                // round dot and the smear sits behind it. Fading by the
                // fraction instead eats the back half of the dot, which reads
                // as a glow leading the star rather than trailing it.
                let back = (1.0 - sd.y) * (sr - pr);
                let decay = exp(-max(back - w, 0.0) / (0.35 * (sr - pr) + 1e-6));
                let glow = (exp(-sd.x * sd.x / (w * w))                 // core
                         + 0.04 * exp(-sd.x * sd.x / (6.0 * w * w)))    // halo
                         * decay;

                // Near-white, with a faint per-star tint so the field isn't flat.
                let tint = 0.92 + 0.08 * cos(vary.y * tau + vec3<f32>(0.0, 2.094, 4.188));

                // Right at the vanishing point even a wide row window cannot
                // cover a star, so fade out the last of it rather than draw
                // the slivers that would be left. The ramp is long and gentle:
                // a short one leaves a dark disc with a visible rim, where
                // this just reads as the field thinning into the distance.
                let core = smoothstep(0.030, 0.048, sr);

                col += tint * glow * show * core * mag
                     * clamp(0.55 + near / 15.0, 0.55, 1.0);
            }
        }
    }

    return vec4<f32>(col, 1.0);
}
