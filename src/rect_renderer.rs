/// rect_renderer.rs — Pipeline wgpu para desenhar retângulos coloridos.
///
/// Usado para os backgrounds dos blocos e do input box.
/// Cada retângulo é 2 triângulos (6 vértices) com posição e cor em NDC.
/// Upload via staging buffer por frame — zero alocação de GPU objects por rect.
use wgpu::util::DeviceExt;
use bytemuck;

/// Um retângulo a ser renderizado (coordenadas de tela em pixels).
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [f32; 4], // RGBA linear
    pub radius: f32,
    pub border_width: f32,
    pub border_color: [f32; 4],
}

/// Vértice enviado ao shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
    // Localização relativa ao centro do retângulo, em pixels.
    // Usado no fragment shader para calcular o SDF de bordas arredondadas.
    local_pos: [f32; 2],
    size: [f32; 2],
    radius: f32,
    border_width: f32,
    border_color: [f32; 4],
}

const SHADER: &str = r#"
struct VertexInput {
    @location(0) pos:          vec2<f32>,
    @location(1) color:        vec4<f32>,
    @location(2) local_pos:    vec2<f32>,
    @location(3) size:         vec2<f32>,
    @location(4) radius:       f32,
    @location(5) border_width: f32,
    @location(6) border_color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_pos:     vec4<f32>,
    @location(0)       color:        vec4<f32>,
    @location(1)       local_pos:    vec2<f32>,
    @location(2)       size:         vec2<f32>,
    @location(3)       radius:       f32,
    @location(4)       border_width: f32,
    @location(5)       border_color: vec4<f32>,
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_pos     = vec4<f32>(in.pos, 0.0, 1.0);
    out.color        = in.color;
    out.local_pos    = in.local_pos;
    out.size         = in.size;
    out.radius       = in.radius;
    out.border_width = in.border_width;
    out.border_color = in.border_color;
    return out;
}

fn rounded_box_sdf(center_rel_pos: vec2<f32>, size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(center_rel_pos) - size + radius;
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - radius;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // SDF do retângulo arredondado.
    // in.size é a metade da largura/altura real.
    let dist = rounded_box_sdf(in.local_pos, in.size * 0.5, in.radius);
    
    // Antialiasing suave usando fwidth
    let smoothing = fwidth(dist);
    let alpha = 1.0 - smoothstep(-smoothing, smoothing, dist);
    
    // Borda
    let border_alpha = 1.0 - smoothstep(in.border_width - smoothing, in.border_width + smoothing, abs(dist + in.border_width * 0.5));
    
    var final_color = in.color;
    if (in.border_width > 0.0) {
        let b_alpha = 1.0 - smoothstep(-smoothing, smoothing, dist + in.border_width);
        // Mistura a cor do fundo com a cor da borda baseado na distância
        let is_border = smoothstep(-in.border_width - smoothing, -in.border_width + smoothing, dist);
        final_color = mix(in.color, in.border_color, is_border);
    }

    return vec4<f32>(final_color.rgb, final_color.a * alpha);
}
"#;

pub struct RectRenderer {
    pipeline: wgpu::RenderPipeline,
    vertices: Vec<Vertex>,
}

impl RectRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect_shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rect_pipeline_layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, // pos
                        1 => Float32x4, // color
                        2 => Float32x2, // local_pos
                        3 => Float32x2, // size
                        4 => Float32,   // radius
                        5 => Float32,   // border_width
                        6 => Float32x4  // border_color
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self { pipeline, vertices: Vec::new() }
    }

    pub fn begin_frame(&mut self) {
        self.vertices.clear();
    }

    pub fn push(&mut self, rect: Rect, screen_w: f32, screen_h: f32) {
        let to_ndc_x = |x: f32| (x / screen_w) * 2.0 - 1.0;
        let to_ndc_y = |y: f32| 1.0 - (y / screen_h) * 2.0;

        let x0 = to_ndc_x(rect.x);
        let x1 = to_ndc_x(rect.x + rect.w);
        let y0 = to_ndc_y(rect.y);
        let y1 = to_ndc_y(rect.y + rect.h);
        
        let half_w = rect.w / 2.0;
        let half_h = rect.h / 2.0;

        let verts = [
            Vertex { 
                pos: [x0, y0], color: rect.color, 
                local_pos: [-half_w, -half_h], size: [rect.w, rect.h], 
                radius: rect.radius, border_width: rect.border_width, border_color: rect.border_color 
            },
            Vertex { 
                pos: [x1, y0], color: rect.color, 
                local_pos: [half_w, -half_h], size: [rect.w, rect.h], 
                radius: rect.radius, border_width: rect.border_width, border_color: rect.border_color 
            },
            Vertex { 
                pos: [x0, y1], color: rect.color, 
                local_pos: [-half_w, half_h], size: [rect.w, rect.h], 
                radius: rect.radius, border_width: rect.border_width, border_color: rect.border_color 
            },
            Vertex { 
                pos: [x1, y0], color: rect.color, 
                local_pos: [half_w, -half_h], size: [rect.w, rect.h], 
                radius: rect.radius, border_width: rect.border_width, border_color: rect.border_color 
            },
            Vertex { 
                pos: [x1, y1], color: rect.color, 
                local_pos: [half_w, half_h], size: [rect.w, rect.h], 
                radius: rect.radius, border_width: rect.border_width, border_color: rect.border_color 
            },
            Vertex { 
                pos: [x0, y1], color: rect.color, 
                local_pos: [-half_w, half_h], size: [rect.w, rect.h], 
                radius: rect.radius, border_width: rect.border_width, border_color: rect.border_color 
            },
        ];
        self.vertices.extend_from_slice(&verts);
    }

    /// Desenha todos os retângulos enfileirados.
    /// `vbuf` deve ser criado antes de abrir o render pass via `build_buffer()`.
    pub fn build_buffer(&self, device: &wgpu::Device) -> Option<wgpu::Buffer> {
        if self.vertices.is_empty() { return None; }
        Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rect_vbuf"),
            contents: bytemuck::cast_slice(&self.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        }))
    }

    pub fn draw<'a>(
        &'a self,
        vbuf: &'a wgpu::Buffer,
        pass: &mut wgpu::RenderPass<'a>,
    ) {
        if self.vertices.is_empty() { return; }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.draw(0..self.vertices.len() as u32, 0..1);
    }
}
