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
use rune::render::{KIND_GLYPH, Renderer};
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
    /// 离屏 float 通道专用渲染器:f.ui 的 uber 管线按表面格式编译,
    /// 画不进 Rgba16Float——base/text 两路必须用同格式的独立管线。
    ui16: Renderer,
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

// 合成输出是 sRGB 编码值;overlay 表面是 sRGB 格式,硬件写入时
// 还会再做一次 linear→sRGB——不先解码回去,中间调全部被双重
// 编码提亮(实测胶囊 (119,176,243) 而非设计蓝 (47,111,237))。
// 上游参考实现 gfx.rs 的 fs_resolve_srgb 同样先 from_srgb 再写。
fn from_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs(v: Vary) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(lit));
    let px = vec2<i32>(v.uv * vec2<f32>(dims));
    let l = textureLoad(lit, px, 0);
    let a = max(textureLoad(base, px, 0).a, textureLoad(text, px, 0).a);
    // 合成输出 alpha 恒 1(不透明窗口设计);overlay 需要底图 alpha
    // 贴回,并转预乘——透明区保持透明,光只在有表面的地方存在。
    return vec4<f32>(from_srgb(l.rgb) * a, a);
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
    emitters: Vec<([f32; 4], [f32; 3], f32, f32, f32, f32, f32)>,  // rect, rgb, k, intensity, radius, alpha, z_pt
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
            // 辐射/灯色转线性(spike 验证过的契约):实例填充是 srgb
            // 值,直接当线性会让红光通道高估数倍、光偏白。
            let lin = |c: f32| {
                if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
            };
            let mut rgb = [lin(fill[0]), lin(fill[1]), lin(fill[2])];
            if mark == MARK_EMITTER_WARM {
                rgb = WARM_EMIT;
            }
            // 角色参数:胶囊/暖月要光池(GI 辐射),天体要盘形。
            // 三族参数:胶囊是唯一光源(full 量级);天体要盘形(低
            // emission 免冲白);暖月是装饰氛围灯——工具栏整条都在
            // 近场,full 量级会把瓷面洗成暖黄浴室,收弱收小。
            // z 是灯离面的高度:胶囊/暖月是贴在瓷面上的元件,灯在
            // 28pt;天体是「天上」的远物——spike 验证过 z 300~520
            // 才是柔光,28pt 等于灯泡贴墙,近场直射把窗角洗白。
            let (gain, intensity, radius, z) = if mark == MARK_EMITTER_SKY {
                (1.15, 2.2, 36.0, 340.0)
            } else if mark == MARK_EMITTER_WARM {
                // 暖月纯 emission:bar 太小,同位点光全是近场直射,
                // 洗白按钮;GI 光池衰减柔,月亮自身亮起来,暖意
                // 只在附近瓷面荡开——装饰灯的本分。
                (1.6, 0.0, 7.0, LIGHT_Z_PT)
            } else {
                (EMIT_GAIN, LIGHT_INTENSITY, LIGHT_RADIUS_PT, LIGHT_Z_PT)
            };
            // 一盏灯的守恒:胶囊是「一个」光源,不管它在条态是小
            // 药丸还是在 column 布局拉成整行横条——总辐射功率恒定,
            // 面积越大单位面积辐射越弱,否则整行横条的辐射总量是小
            // 药丸的十几倍,GI 光池按比例泛滥(column-stress 实机整窗
            // 洗蓝)。参考面积取条态胶囊的典型值;小胶囊不放大(clamp 1.0)。
            let area_pt = (rect[2] / k) * (rect[3] / k);
            let power_scale = (4000.0 / area_pt.max(1.0)).min(1.0);
            let gain = gain * power_scale;
            out.prims.push(
                Prim::from_rune(rect, radii)
                    .albedo(rgb)
                    .raised(2.0 * k)
                    .bevel(4.2 * k, 2.7 * k)
                    .gloss(0.85, 0.15)
                    .emissive([rgb[0] * gain, rgb[1] * gain, rgb[2] * gain]),
            );
            // fill[3] 是 alpha:日出日落天体渐隐时,掺白与强度必须
            // 跟暗——否则隐形的天体还在投满强度的白光,光与源脱节。
            out.emitters.push((rect, rgb, k, intensity, radius, fill[3], z));
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
        // 实例是设备 px,常量是 pt:帧级倍率从 size/viewport 推
        // (params[3] 是字形覆盖率指数,不是倍率——曾按它取值导致
        // z/radius/range 全部归零,光照全黑)。
        // mark 继承是上游设计("最近标记节点"):壳容器里每个后代实例
        // 都带着 MARK_SHELL——字形方块、行底、箭头全部算在内。提取
        // 必须讲策略:字形排除(文字走文字纹理,不是 3D 形体);隐形
        // 实例跳过(alpha≈0 的填充不挡光);壳是面板自己的背景——
        // 非字形实例里面积最大的那个;emitter 按矩形去重(填充+描边
        // 两个实例同几何只取一份)。
        let k = if f.rt.viewport.0 > 1.0 {
            (f.size.0 as f32 / f.rt.viewport.0).clamp(0.5, 4.0)
        } else {
            1.0
        };
        let mut shell: Option<(f32, usize)> = None;
        let mut emitter_rects: Vec<[f32; 4]> = Vec::new();
        for (n, i) in f.rt.instances.iter().enumerate() {
            let glyph = i.params[1] == KIND_GLYPH;
            if glyph {
                text_idx.push(n);
            } else {
                base_idx.push(n);
            }
            let mark = f.rt.marks.get(n).copied().unwrap_or(0) & 0xff;
            if glyph || i.fill[3] < 0.01 {
                continue;
            }
            match mark {
                MARK_SHELL => {
                    any_mark = true;
                    let area = i.rect[2] * i.rect[3];
                    if shell.is_none_or(|(a, _)| area > a) {
                        shell = Some((area, n));
                    }
                }
                MARK_EMITTER | MARK_EMITTER_WARM | MARK_EMITTER_SKY => {
                    any_mark = true;
                    if !emitter_rects.iter().any(|r| {
                        (r[0] - i.rect[0]).abs() < 0.5
                            && (r[1] - i.rect[1]).abs() < 0.5
                            && (r[2] - i.rect[2]).abs() < 0.5
                            && (r[3] - i.rect[3]).abs() < 0.5
                    }) {
                        emitter_rects.push(i.rect);
                        extract(i.rect, i.radii, i.fill, mark, k, &mut extracted);
                    }
                }
                _ => {}
            }
        }
        if let Some((_, n)) = shell {
            // 壳先于 emitter 组装:gbuffer 画家序后者赢,胶囊 z 盖住
            // 面板才能触发自遮挡——否则胶囊被自己的同位灯直射炸白
            // (spike 对照实验:prim 翻序胶囊中心 216 vs 80)。
            let i = f.rt.instances[n];
            let mut ordered = Extracted { prims: Vec::new(), emitters: Vec::new() };
            extract(i.rect, i.radii, i.fill, MARK_SHELL, k, &mut ordered);
            ordered.prims.extend(extracted.prims.iter().copied());
            ordered.emitters = extracted.emitters;
            extracted = ordered;
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
            .map(|(rect, rgb, k, intensity, radius, alpha, z)| {
                let cx = rect[0] + rect[2] / 2.0;
                let cy = rect[1] + rect[3] / 2.0;
                Light::point(cx, cy, z * k)
                    .color([
                        rgb[0] + 0.25 * alpha,
                        rgb[1] + 0.28 * alpha,
                        rgb[2] + 0.32 * alpha,
                    ])
                    .intensity(*intensity)
                    .radius(radius * k)
                    .range(LIGHT_RANGE_PT * k)
            })
            .collect();
        let lights = emitter_lights;
        // 无灯帧(如无选中胶囊的条):rune-light 无灯早退、lit 不写入,
        // 直接原始绘制——无灯即无光效,平面底图就是正确的物理结果。
        if lights.is_empty() {
            f.ui.prepare(f.device, f.queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);
            f.ui.draw(f.encoder, f.target, f.rt.clear_color());
            return;
        }

        let (device, queue, size) = (f.device, f.queue, f.size);
        let device_key = f.device_key();
        let format = f.format;
        if self.gpu.as_ref().is_none_or(|g| {
            g.device_key != device_key || g.size != size || g.format != format
        }) {
            let light = LightRenderer::new(device, queue, size.0, size.1, wgpu::TextureFormat::Rgba8Unorm);
            let (blit, bind_layout) = blit_pipeline(device, format);
            self.gpu = Some(Gpu {
                device_key,
                size,
                format,
                ui16: Renderer::new(device, wgpu::TextureFormat::Rgba16Float),
                light,
                base: float_target(device, size, "light-base"),
                text: float_target(device, size, "light-text"),
                lit: device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("light-lit"),
                        size: wgpu::Extent3d {
                            width: size.0.max(1),
                            height: size.1.max(1),
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&Default::default()),
                blit,
                bind_layout,
            });
        }
        let gpu = self.gpu.as_mut().unwrap();

        // 底图(非文字)与文字两路离屏。
        let base_instances: Vec<_> = base_idx.iter().map(|n| f.rt.instances[*n]).collect();
        let text_instances: Vec<_> = text_idx.iter().map(|n| f.rt.instances[*n]).collect();
        gpu.ui16.prepare(device, queue, &mut f.rt.text.atlas, &base_instances, size);
        {
            let mut enc = device.create_command_encoder(&Default::default());
            gpu.ui16.draw(&mut enc, &gpu.base, [0.0; 4]);
            queue.submit([enc.finish()]);
        }
        gpu.ui16.prepare(device, queue, &mut f.rt.text.atlas, &text_instances, size);
        {
            let mut enc = device.create_command_encoder(&Default::default());
            gpu.ui16.draw(&mut enc, &gpu.text, [0.0; 4]);
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
