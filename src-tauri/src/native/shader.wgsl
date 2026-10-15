// The native renderer's shaders: Lambert lighting with a hemisphere sky light, a sun
// and a lamp, linear-distance fog, a gradient sky, instanced trees and text billboards.
// Light levels follow three.js (diffuse = colour × irradiance / π) so both renderers match.

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    camera: vec4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    hemi_sky: vec4<f32>,
    hemi_ground: vec4<f32>,
    fog_color: vec4<f32>,
    // near, far
    fog: vec4<f32>,
    // xyz position, w intensity
    lamp: vec4<f32>,
    sky_top: vec4<f32>,
    sky_bottom: vec4<f32>,
    cam_right: vec4<f32>,
    cam_up: vec4<f32>,
};

struct Draw {
    model: mat4x4<f32>,
    tint: vec4<f32>,
    // x: lit (1) or unlit (0); y: jersey mode (colour + uv.x × tint); z: flat shading; w: plain white (ghosts)
    flags: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
@group(2) @binding(0) var<uniform> d: Draw;

const PI: f32 = 3.14159265;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec3<f32>, @location(3) uv: vec2<f32>) -> VsOut {
    var o: VsOut;
    let world = d.model * vec4<f32>(pos, 1.0);
    o.world = world.xyz;
    o.normal = (d.model * vec4<f32>(normal, 0.0)).xyz;
    o.color = color;
    o.uv = uv;
    o.clip = g.view_proj * world;
    return o;
}

/// A tree: one shared cone-and-trunk mesh placed by `[x, y, z, yaw, width, height]`.
@vertex
fn vs_tree(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec3<f32>, @location(3) uv: vec2<f32>,
           @location(4) at: vec3<f32>, @location(5) shape: vec3<f32>) -> VsOut {
    var o: VsOut;
    let c = cos(shape.x);
    let s = sin(shape.x);
    let p = vec3<f32>(pos.x * shape.y, pos.y * shape.z, pos.z * shape.y);
    let r = vec3<f32>(c * p.x + s * p.z, p.y, -s * p.x + c * p.z);
    let world = r + at + d.model[3].xyz;
    o.world = world;
    o.normal = vec3<f32>(c * normal.x + s * normal.z, normal.y, -s * normal.x + c * normal.z);
    o.color = color;
    o.uv = uv;
    o.clip = g.view_proj * vec4<f32>(world, 1.0);
    return o;
}

fn fogged(c: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
    let dist = distance(world, g.camera.xyz);
    let f = smoothstep(g.fog.x, g.fog.y, dist);
    return mix(c, g.fog_color.rgb, f);
}

@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, samp, i.uv);
    var base = i.color * d.tint.rgb * texel.rgb;
    if (d.flags.y > 0.5) {
        // Rider models: the jersey's share of each vertex takes the rider's colour.
        base = i.color + i.uv.x * d.tint.rgb;
    }
    if (d.flags.w > 0.5) {
        base = vec3<f32>(1.0);
    }
    var lit = base;
    if (d.flags.x > 0.5) {
        var n = normalize(i.normal);
        if (d.flags.z > 0.5) {
            // Flat shading, facing the viewer (the camera is at the origin).
            n = normalize(cross(dpdx(i.world), dpdy(i.world)));
            if (dot(n, -i.world) < 0.0) { n = -n; }
        }
        let hemi = mix(g.hemi_ground.rgb, g.hemi_sky.rgb, 0.5 * n.y + 0.5);
        let sun = g.sun_color.rgb * max(dot(n, g.sun_dir.xyz), 0.0);
        let to_lamp = g.lamp.xyz - i.world;
        let ld = length(to_lamp);
        let falloff = 1.0 / max(pow(ld, 1.6), 0.01) * pow(clamp(1.0 - pow(ld / 32.0, 4.0), 0.0, 1.0), 2.0);
        let lamp = vec3<f32>(1.0, 0.957, 0.878) * g.lamp.w * falloff * max(dot(n, to_lamp / max(ld, 0.001)), 0.0);
        lit = base * (hemi + sun + lamp) / PI;
    }
    return vec4<f32>(fogged(lit, i.world), d.tint.a * texel.a);
}

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_sky(@builtin(vertex_index) v: u32) -> SkyOut {
    var o: SkyOut;
    let xy = vec2<f32>(f32((v << 1u) & 2u), f32(v & 2u)) * 2.0 - 1.0;
    o.clip = vec4<f32>(xy, 0.0, 1.0);
    o.ndc = xy;
    return o;
}

@fragment
fn fs_sky(i: SkyOut) -> @location(0) vec4<f32> {
    let far = g.inv_view_proj * vec4<f32>(i.ndc, 1.0, 1.0);
    let dir = normalize(far.xyz / far.w - g.camera.xyz);
    let h = clamp(dir.y, 0.0, 1.0);
    return vec4<f32>(mix(g.sky_bottom.rgb, g.sky_top.rgb, pow(h, 0.55)), 1.0);
}

struct LabelOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec3<f32>,
};

/// A camera-facing text sign: centre, corner (−1…1), size in metres and atlas coordinates.
@vertex
fn vs_label(@location(0) centre: vec3<f32>, @location(1) corner: vec2<f32>, @location(2) size: vec2<f32>, @location(3) uv: vec2<f32>) -> LabelOut {
    var o: LabelOut;
    let world = centre + g.cam_right.xyz * corner.x * size.x * 0.5 + g.cam_up.xyz * corner.y * size.y * 0.5;
    o.clip = g.view_proj * vec4<f32>(world, 1.0);
    o.uv = uv;
    o.world = world;
    return o;
}

@fragment
fn fs_label(i: LabelOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, i.uv);
    if (c.a < 0.02) { discard; }
    return vec4<f32>(fogged(c.rgb, i.world), c.a);
}
