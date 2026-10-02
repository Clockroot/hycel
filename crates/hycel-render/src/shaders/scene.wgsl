struct Camera {
    view_projection: mat4x4<f32>,
    screen_projection: mat4x4<f32>,
};

@group(0) @binding(0) var scene_sampler: sampler;
@group(0) @binding(1) var scene_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) screen_space: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) screen_space: f32,
};

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world_position = camera.view_projection * vec4<f32>(input.position, 0.0, 1.0);
    let screen_position = camera.screen_projection * vec4<f32>(input.position, 0.0, 1.0);
    output.clip_position = select(world_position, screen_position, input.screen_space > 0.5);
    output.uv = input.uv;
    output.color = input.color;
    output.screen_space = input.screen_space;
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(scene_texture, scene_sampler, input.uv) * input.color;
}
