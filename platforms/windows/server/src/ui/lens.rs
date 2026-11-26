//! 液态玻璃的 GPU 光学:rune-fx lens 经 effect slot 挂进候选窗与
//! 工具栏的 overlay。背板是抓屏薄层原样铺进窗口的桌面像素,折射、
//! 色散、frost 模糊与 rim 光全部交给上游维护的 lens 着色器;这里
//! 只管把 slot 的矩形与圆角翻译成 lens 形状,并按 `ui_glass_frost`
//! 连续插值参数。
//!
//! 形状跟随是免费的:slot 跟随元素的 presented rect,展开/收起的
//! 布局弹簧走到哪儿,玻璃形体跟到哪儿,不需要任何手写同步。

use rune::compose::{Compositor, DeviceKey, Frame};
use rune::render::{EffectPhase, EffectSite, KIND_EFFECT};
use rune_fx::{Fx, FxFrame, FxLayer, FxOptions, FxRenderer, FxSlot, LensParams, Shape, rgb};

/// 玻璃 lens 的 slot 标记;一个 overlay 里只有这一种 effect。
pub(crate) const LENS: u32 = 1;

/// `ui_glass_frost` 0..1 的连续插值:0 是高透(仍留 σ3.5 的雾化与
/// 0.35 的深色 veil 保底文字可读),1 是磨砂。折射/色散/rim 不随
/// 滑杆退坡——它们是玻璃的本体,磨砂端只是被更强的雾化盖住。
pub(crate) fn lens_params(frost: f32) -> LensParams {
    let frost = frost.clamp(0.0, 1.0);
    LensParams {
        bevel: 14.0,
        depth: 18.0,
        dispersion: 0.16,
        magnification: 1.05,
        frost: 0.25 + 0.55 * frost,
        frost_sigma: 14.0,
        veil: 0.35 + 0.27 * frost,
        tint: rgb(0x0e1014),
        rim: 1.4,
        shadow: 0.3,
        ..Default::default()
    }
}

/// 把一条 effect slot 的 presented 几何翻译成 lens 层。`data[0]` 是
/// frost;返回 None 表示这不是玻璃 slot。
fn layer(kind: u32, data: [f32; 4], shape: Shape) -> Option<FxLayer> {
    if kind != LENS {
        return None;
    }
    Some(Fx::lens(lens_params(data[0])).on(shape).at(0.0))
}

struct FxCache {
    device_key: DeviceKey,
    format: wgpu::TextureFormat,
    fx: FxRenderer,
}

/// 玻璃 overlay 的 compositor:每帧把控件树里的 effect slot 换成
/// lens 层,backdrop 拷贝经 `draw_with_backdrop` 喂给 frost 模糊。
/// 无状态设备缓存;GPU 丢失重建由 `release_gpu` 配合。
pub(crate) struct LensFx {
    fx: Option<FxCache>,
}

impl LensFx {
    pub(crate) fn new() -> Self {
        Self { fx: None }
    }

    pub(crate) fn release(&mut self) {
        self.fx = None;
    }
}

impl<S: 'static> Compositor<S> for LensFx {
    fn animating(&self) -> bool {
        false
    }

    fn release_gpu(&mut self) {
        self.release();
    }

    fn render(&mut self, f: &mut Frame<'_, S>) {
        let (device, queue, size) = (f.device, f.queue, f.size);
        let mut at: Vec<u32> = Vec::new();
        let mut layers: Vec<FxLayer> = Vec::new();
        let mut placements: Vec<FxSlot> = Vec::new();
        for (n, i) in f.rt.instances.iter().enumerate() {
            if i.params[1] != KIND_EFFECT {
                continue;
            }
            let kind = f.rt.marks.get(n).copied().unwrap_or(0) & 0xff;
            let k = i.params[3];
            let [x, y, w, h] = i.rect.map(|v| v / k);
            let r = i.radii.iter().fold(0.0f32, |a, v| a.max(v.abs())) / k;
            let Some(l) = layer(kind, i.uv, Shape::rounded(x, y, w, h, r)) else {
                continue;
            };
            at.push(n as u32);
            layers.push(l);
            placements.push(FxSlot::default().scale(k).clip(i.clip, i.fade));
        }
        if layers.is_empty() {
            f.ui.prepare(device, queue, &mut f.rt.text.atlas, &f.rt.instances, size);
            f.ui.draw(f.encoder, f.target, f.rt.clear_color());
            return;
        }
        let device_key = f.device_key();
        if self
            .fx
            .as_ref()
            .is_none_or(|c| c.device_key != device_key || c.format != f.format)
        {
            self.fx = None;
            let mut fx = FxRenderer::new(device, f.format, FxOptions { dither: true });
            fx.add_format(device, wgpu::TextureFormat::Rgba16Float);
            self.fx = Some(FxCache {
                device_key,
                format: f.format,
                fx,
            });
        }
        let fx = &mut self.fx.as_mut().unwrap().fx;
        fx.set_srgb_blend(f.srgb_blend());
        fx.prepare_with_slots(
            device,
            queue,
            &FxFrame {
                viewport: [size.0, size.1],
                scale: 1.0,
                layers: &layers,
                backdrop: None,
                sources: &[],
            },
            &placements,
        );
        let clear = f.rt.clear_color();
        f.ui.prepare(device, queue, &mut f.rt.text.atlas, &f.rt.instances, size);
        f.ui.draw_with_backdrop(
            f.encoder,
            f.target,
            clear,
            &mut |phase, s: &EffectSite<'_>| {
                let Some(k) = at.iter().position(|n| *n == s.index) else {
                    return;
                };
                match phase {
                    EffectPhase::Backdrop(enc) => {
                        if let Some(bd) = s.backdrop {
                            fx.blur_backdrop(device, queue, enc, bd, k);
                        }
                    }
                    EffectPhase::Draw(pass) => fx.draw_layer_in(pass, s.format, s.origin, k),
                }
            },
        );
    }
}

/// 材质分派 compositor:候选窗/工具栏共用。玻璃帧(有 lens slot)走
/// lens 光学;黑瓷帧(有光照标记)走 rune-light 物理光照;都没有时
/// 原始绘制。两种材质按配置互斥,不会同帧出现。
pub(crate) struct SurfaceFx {
    lens: LensFx,
    light: lueying_server::lighting::LightFx,
}

impl SurfaceFx {
    pub(crate) fn new() -> Self {
        Self {
            lens: LensFx::new(),
            light: lueying_server::lighting::LightFx::new(),
        }
    }
}

impl<S: 'static> Compositor<S> for SurfaceFx {
    fn animating(&self) -> bool {
        false
    }

    fn release_gpu(&mut self) {
        self.lens.release();
        self.light.release();
    }

    fn render(&mut self, f: &mut Frame<'_, S>) {
        let mut has_lens = false;
        let mut has_light = false;
        for (n, i) in f.rt.instances.iter().enumerate() {
            let mark = f.rt.marks.get(n).copied().unwrap_or(0) & 0xff;
            if i.params[1] == KIND_EFFECT && mark == LENS {
                has_lens = true;
            }
            if matches!(
                mark,
                lueying_server::lighting::MARK_SHELL
                    | lueying_server::lighting::MARK_EMITTER
                    | lueying_server::lighting::MARK_EMITTER_WARM
            ) {
                has_light = true;
            }
        }
        if has_lens {
            self.lens.render(f);
        } else if has_light {
            self.light.render(f);
        } else {
            f.ui.prepare(
                f.device,
                f.queue,
                &mut f.rt.text.atlas,
                &f.rt.instances,
                f.size,
            );
            f.ui.draw(f.encoder, f.target, f.rt.clear_color());
        }
    }
}
