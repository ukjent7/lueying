//! 黑瓷的物理光照:rune-light 经 compositor 挂进候选窗/工具栏 overlay
//! 与设置窗。光是真的——光源位置 3D、bevel 法线决定棱缘吃光、
//! gloss/roughness 决定高光形状,全部上游维护;这里只做三件平台 plumbing:
//!
//! 1. 按 KIND_GLYPH 把实例流拆成底图/文字两路(rune-light 的输入契约);
//! 2. 按元素 mark 提取光照 primitive 与光源:发光元件(选中胶囊、暖月)
//!    的中心就是灯位——胶囊飞,光跟着飞,零手写同步;
//! 3. overlay alpha 修正:rune-light 的合成着色器固定输出 alpha=1(为
//!    不透明窗口设计),透明 overlay 需要把底图 alpha 贴回去,否则透明
//!    区变纯黑。这是一遍全屏 blit,不是材质算法。
//!
//! 光源哲学:画面里每一束光都必须能指回一个真实发光的元件。无灯、
//! 无 emitter 时 render 零开销,compositor 也直接走原始绘制。

use rune::compose::{Compositor, DeviceKey, Frame};
use rune::render::KIND_GLYPH;
use rune_light::{Gi, Light, LightRenderer, Prim, Scene, Settings, TextPolicy};

/// 瓷面(面板外壳):raised + bevel + gloss 的受光面。
pub const MARK_SHELL: u32 = 2;
/// 发光元件(选中胶囊):emissive 且其中心为灯位;辐射色取实例填充。
pub const MARK_EMITTER: u32 = 3;
/// 暖色发光体(暖月):同色路但辐射色固定暖黄——月亮是自己的天体,
/// 不跟主题色走,而渐变盘的实例填充不一定可取。
pub const MARK_EMITTER_WARM: u32 = 4;
/// 天体发光体(设置窗日/月):低 emission 增益保盘形——lit_room 模式
/// 无 GI,emission 只决定盘自身亮度,光池全由同位点光负责;增益高
/// 了盘心冲白、天体糊成灯泡。
pub const MARK_EMITTER_SKY: u32 = 5;
/// 暖月定色(0xfac738 线性值)。发光强度与胶囊同倍数。
const WARM_EMIT: [f32; 3] = [0.91, 0.66, 0.04];

/// 发光强度:emission 与点光共用元件填充色的倍数。accent 实体发光,
/// 掺白降饱和——瓷要的是釉面反射,不是霓虹。
const EMIT_GAIN: f32 = 2.6;
/// 点光贴着元件上方的高度(pt)与半径:小半径→紧实高光(瓷)。
const LIGHT_Z_PT: f32 = 28.0;
const LIGHT_RADIUS_PT: f32 = 9.0;
const LIGHT_INTENSITY: f32 = 1.9;
const LIGHT_RANGE_PT: f32 = 420.0;
/// 环境底:不是光源,是曝光基准——房间不是绝对黑,远端文字不死。
const AMBIENT: [f32; 3] = [0.08, 0.08, 0.085];

struct Gpu {
    device_key: DeviceKey,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    light: LightRenderer,
    base: wgpu::TextureView,
    text: wgpu::TextureView,
    lit: wgpu::TextureView,
    blit: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
}

/// 光源策略:候选窗/工具栏是暗室戏剧(元件独秀);设置窗是亮室
/// 工具(高 ambient 保底)。两者的灯都由发光元件驱动——胶囊、暖月、
/// 窗顶看得见的日月。
#[derive(Clone, Copy, PartialEq)]
pub enum Source {
    /// 发光元件中心即灯位;灯随元件飞行。
    Emitters,
    /// 同 emitter 驱动,但 ambient 抬高、GI 关闭(工具窗)。
    LitRoom([f32; 3]),
}

/// 光照 compositor。无光照标记的帧走原始绘制,零额外开销。
pub struct LightFx {
    gpu: Option<Gpu>,
    source: Source,
}

impl LightFx {
    pub fn new() -> Self {
        Self { gpu: None, source: Source::Emitters }
    }

    /// 亮室 emitter 模式:高 ambient 保底可读、关 GI。设置窗用——
    /// 日月 emitter 驱动光照,但工具窗任何时刻都要均匀可读。
    pub fn lit_room(ambient: [f32; 3]) -> Self {
        Self { gpu: None, source: Source::LitRoom(ambient) }
    }

    pub fn release(&mut self) {
        self.gpu = None;
    }
}


fn float_target(device: &wgpu::Device, size: (u32, u32), label: &str) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

const BLIT_WGSL: &str = r"
@group(0) @binding(0) var lit: texture_2d<f32>;
@group(0) @binding(1) var base: texture_2d<f32>;
@group(0) @binding(2) var text: texture_2d<f32>;

struct Vary {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> Vary {
    // 全屏三角:三个超大顶点覆盖整个目标。
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var v: Vary;
    let xy = p[vi];
    v.pos = vec4<f32>(xy, 0.0, 1.0);
    v.uv = xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return v;
}

@fragment
fn fs(v: Vary) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(lit));
    let px = vec2<i32>(v.uv * vec2<f32>(dims));
    let l = textureLoad(lit, px, 0);
    let a = max(textureLoad(base, px, 0).a, textureLoad(text, px, 0).a);
    // 合成输出 alpha 恒 1(不透明窗口设计);overlay 需要底图 alpha
    // 贴回,并转预乘——透明区保持透明,光只在有表面的地方存在。
    return vec4<f32>(l.rgb * a, a);
}
";

fn blit_pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("lueying-light-blit"),
        source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
    });
    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("lueying-light-blit"),
        entries: &[0, 1, 2].map(|i| wgpu::BindGroupLayoutEntry {
            binding: i,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("lueying-light-blit"),
        bind_group_layouts: &[Some(&bind_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("lueying-light-blit"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // 源已是预乘:直接覆盖式合成(目标先是透明 clear)。
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bind_layout)
}

/// 一帧的光照提取:发光元件中心(pt→设备 px 转换在调用处)与 prim 列表。
struct Extracted {
    prims: Vec<Prim>,
    emitters: Vec<([f32; 4], [f32; 3], f32, f32, f32)>,  // rect, rgb, k, intensity, radius
}

/// 从实例流提取瓷面与发光元件。实例矩形是设备 px;辐射色取实例填充。
fn extract(rect: [f32; 4], radii: [f32; 4], fill: [f32; 4], mark: u32, k: f32, out: &mut Extracted) {
    match mark {
        MARK_SHELL => {
            out.prims.push(
                Prim::from_rune(rect, radii)
                    .albedo([fill[0], fill[1], fill[2]])
                    .raised(3.0 * k)
                    .bevel(6.0 * k, 3.6 * k)
                    .gloss(0.85, 0.15),
            );
        }
        MARK_EMITTER | MARK_EMITTER_WARM | MARK_EMITTER_SKY => {
            let mut rgb = [fill[0], fill[1], fill[2]];
            if mark == MARK_EMITTER_WARM {
                rgb = WARM_EMIT;
            }
            // 角色参数:胶囊/暖月要光池(GI 辐射),天体要盘形。
            let (gain, intensity, radius) = if mark == MARK_EMITTER_SKY {
                (1.15, 1.55, 11.0)
            } else {
                (EMIT_GAIN, LIGHT_INTENSITY, LIGHT_RADIUS_PT)
            };
            out.prims.push(
                Prim::from_rune(rect, radii)
                    .albedo(rgb)
                    .raised(2.0 * k)
                    .bevel(4.2 * k, 2.7 * k)
                    .gloss(0.85, 0.15)
                    .emissive([rgb[0] * gain, rgb[1] * gain, rgb[2] * gain]),
            );
            out.emitters.push((rect, rgb, k, intensity, radius));
        }
        _ => {}
    }
}

impl<S: 'static> Compositor<S> for LightFx {
    fn animating(&self) -> bool {
        false
    }

    fn release_gpu(&mut self) {
        self.release();
    }

    fn render(&mut self, f: &mut Frame<'_, S>) {
        // 扫描实例:拆文字/底图,提取光照 primitive 与发光元件。
        let mut extracted = Extracted { prims: Vec::new(), emitters: Vec::new() };
        let mut base_idx: Vec<usize> = Vec::new();
        let mut text_idx: Vec<usize> = Vec::new();
        let mut any_mark = false;
        for (n, i) in f.rt.instances.iter().enumerate() {
            if i.params[1] == KIND_GLYPH {
                text_idx.push(n);
            } else {
                base_idx.push(n);
            }
            let mark = f.rt.marks.get(n).copied().unwrap_or(0) & 0xff;
            if mark == MARK_SHELL || mark == MARK_EMITTER || mark == MARK_EMITTER_WARM || mark == MARK_EMITTER_SKY {
                any_mark = true;
                extract(i.rect, i.radii, i.fill, mark, i.params[3], &mut extracted);
            }
        }
        if !any_mark {
            f.ui.prepare(f.device, f.queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);
            f.ui.draw(f.encoder, f.target, f.rt.clear_color());
            return;
        }

        // 灯光:Emitters 模式每个发光元件中心一盏同位点光(色掺白降
        // 饱和);Skylight 模式一盏时间驱动的天光,位置相对窗口中心。
        let emitter_lights: Vec<Light> = extracted
            .emitters
            .iter()
            .map(|(rect, rgb, k, intensity, radius)| {
                let cx = rect[0] + rect[2] / 2.0;
                let cy = rect[1] + rect[3] / 2.0;
                Light::point(cx, cy, LIGHT_Z_PT * k)
                    .color([
                        rgb[0] + 0.25,
                        rgb[1] + 0.28,
                        rgb[2] + 0.32,
                    ])
                    .intensity(*intensity)
                    .radius(radius * k)
                    .range(LIGHT_RANGE_PT * k)
            })
            .collect();
        let lights = emitter_lights;

        let (device, queue, size) = (f.device, f.queue, f.size);
        let device_key = f.device_key();
        let format = f.format;
        if self.gpu.as_ref().is_none_or(|g| {
            g.device_key != device_key || g.size != size || g.format != format
        }) {
            let light = LightRenderer::new(device, queue, size.0, size.1, wgpu::TextureFormat::Rgba16Float);
            let (blit, bind_layout) = blit_pipeline(device, format);
            self.gpu = Some(Gpu {
                device_key,
                size,
                format,
                light,
                base: float_target(device, size, "light-base"),
                text: float_target(device, size, "light-text"),
                lit: float_target(device, size, "light-lit"),
                blit,
                bind_layout,
            });
        }
        let gpu = self.gpu.as_mut().unwrap();

        // 底图(非文字)与文字两路离屏。
        let base_instances: Vec<_> = base_idx.iter().map(|n| f.rt.instances[*n]).collect();
        let text_instances: Vec<_> = text_idx.iter().map(|n| f.rt.instances[*n]).collect();
        f.ui.prepare(device, queue, &mut f.rt.text.atlas, &base_instances, size);
        {
            let mut enc = device.create_command_encoder(&Default::default());
            f.ui.draw(&mut enc, &gpu.base, [0.0; 4]);
            queue.submit([enc.finish()]);
        }
        f.ui.prepare(device, queue, &mut f.rt.text.atlas, &text_instances, size);
        {
            let mut enc = device.create_command_encoder(&Default::default());
            f.ui.draw(&mut enc, &gpu.text, [0.0; 4]);
            queue.submit([enc.finish()]);
        }

        let settings = Settings {
            ambient: match self.source {
                Source::LitRoom(a) => a,
                Source::Emitters => AMBIENT,
            },
            albedo_floor: 0.06,
            text: TextPolicy { floor: 0.5, ceiling: 1.3, ..Default::default() },
            gi: if matches!(self.source, Source::Emitters) { Some(Gi::default()) } else { None },
            ..Default::default()
        };
        gpu.light.render(
            device,
            queue,
            f.encoder,
            &Scene { prims: &extracted.prims, lights: &lights, settings, emission: None },
            &gpu.base,
            &gpu.text,
            &gpu.lit,
        );

        // alpha 修正 blit 到 overlay 目标。
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lueying-light-blit"),
            layout: &gpu.bind_layout,
            entries: [
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&gpu.lit) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&gpu.base) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&gpu.text) },
            ]
            .as_slice(),
        });
        // 先清目标为透明,再 blit。
        {
            let mut pass = f.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lueying-light-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: f.target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&gpu.blit);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}
