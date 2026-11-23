//! 候选窗选中胶囊的物理运动。
//!
//! 中心 x/y 与基础宽高各是一条一维阻尼弹簧;形变是独立的有符号
//! 弹簧,由两轴沿行进的加速度差驱动:水平前进拉宽压高、刹车收宽
//! 增高;垂直前进拉高收宽、刹车收高增宽。绘制按 `w * e^{s}, h * e^{-s}`
//! 做 reciprocal scale,包围框面积近似不变,停稳后精确回到目标矩形。

use lueying_render::panel::Rect;

const TAU: f32 = std::f32::consts::TAU;

/// 中心少过冲,尾巴留一小口回弹;尺寸略欠阻尼,宽度变化收尾能看出
/// 一次压回。形变自己再慢半拍,刹车压扁后回圆不抢中心。
const CENTER_PERIOD: f32 = 0.26;
const CENTER_DAMPING: f32 = 0.84;
const SIZE_PERIOD: f32 = 0.26;
const SIZE_DAMPING: f32 = 0.68;
const STRAIN_PERIOD: f32 = 0.20;
const STRAIN_DAMPING: f32 = 0.52;

const CENTER_OMEGA: f32 = TAU / CENTER_PERIOD;
const SIZE_OMEGA: f32 = TAU / SIZE_PERIOD;
const STRAIN_OMEGA: f32 = TAU / STRAIN_PERIOD;
const CENTER_K: f32 = CENTER_OMEGA * CENTER_OMEGA;
const CENTER_C: f32 = 2.0 * CENTER_DAMPING * CENTER_OMEGA;
const SIZE_K: f32 = SIZE_OMEGA * SIZE_OMEGA;
const SIZE_C: f32 = 2.0 * SIZE_DAMPING * SIZE_OMEGA;
const STRAIN_K: f32 = STRAIN_OMEGA * STRAIN_OMEGA;
const STRAIN_C: f32 = 2.0 * STRAIN_DAMPING * STRAIN_OMEGA;

/// 加速度先用 `v/sqrt(v² + v_floor²)` 压掉静止抖动;速度下限取每秒
/// 多少个格短边。增益把一次格间移动的应变压到 ~5%,限幅兜住大步跳。
const STRAIN_FLOOR_SPEED: f32 = 6.0;
const STRAIN_GAIN: f32 = 0.32;
const STRAIN_MAX: f32 = 0.14;

/// 积分步长上限;更长的停帧直接落位,不在旧目标上补跳。
const FULL_STEP: f32 = 1.0 / 240.0;
const SNAP_AFTER: f64 = 0.25;

/// 收尾阈值:足够小就取整,保证停稳后与目标逐位一致。
const POS_EPS: f32 = 0.01;
const VEL_EPS: f32 = 0.5;
const STRAIN_EPS: f32 = 0.001;
const STRAIN_VEL_EPS: f32 = 0.01;
const STRAIN_HARD_LIMIT: f32 = 0.25;

#[derive(Clone, Copy, Debug, Default)]
struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
}

impl Spring {
    fn at(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    fn step(&mut self, k: f32, c: f32, h: f32) {
        let accel = (self.target - self.value) * k - self.velocity * c;
        self.velocity += accel * h;
        self.value += self.velocity * h;
        if !self.value.is_finite() || !self.velocity.is_finite() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    fn accel(&self, k: f32, c: f32) -> f32 {
        (self.target - self.value) * k - self.velocity * c
    }

    fn settle(&mut self) {
        self.value = self.target;
        self.velocity = 0.0;
    }

    fn snap_if_close(&mut self) {
        if (self.target - self.value).abs() <= POS_EPS && self.velocity.abs() <= VEL_EPS {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    fn settled(&self) -> bool {
        (self.target - self.value).abs() <= POS_EPS && self.velocity.abs() <= VEL_EPS
    }
}

/// 选中胶囊的运动状态;所有矩形都是面板本地逻辑点。
#[derive(Clone, Debug, Default)]
pub struct SelectionMotion {
    center_x: Spring,
    center_y: Spring,
    width: Spring,
    height: Spring,
    /// 有符号正交应变:>0 拉长宽、压矮高,<0 反过来。
    strain: Spring,
    target: Option<Rect>,
    /// 本次 retarget 是否换了候选槽。同一槽的文字/布局重算不激励
    /// 形变;旧应变仍按自己的弹簧连续衰减。
    slot_changed: bool,
    /// 上次积分到的时钟(秒)。
    sim_at: f64,
}

impl SelectionMotion {
    /// 首次显示或形态切换:直接落在目标上,不继承旧速度。
    pub fn reset(&mut self, rect: Rect, now: f64) {
        let (cx, cy, w, h) = target_values(rect);
        self.center_x = Spring::at(cx);
        self.center_y = Spring::at(cy);
        self.width = Spring::at(w);
        self.height = Spring::at(h);
        self.strain = Spring::at(0.0);
        self.target = Some(rect);
        self.slot_changed = false;
        self.sim_at = now;
    }

    /// 同页同形态内改目标:先把在途状态推进到事件帧,再换目标;
    /// 位置与速度原样保留。只有长停帧且确有在途运动才直接落位——
    /// 静止很久没有 poll 不是掉帧,下一次按键仍从旧格平滑起步。
    ///
    /// `slot_changed` 为 false 时只改几何(同一候选的文字/布局重算),
    /// 不激励新的形变;已有应变连续衰减,不在这里清零。
    pub fn retarget(&mut self, rect: Rect, now: f64, slot_changed: bool) {
        if self.target.is_none() {
            self.reset(rect, now);
            return;
        }
        let dt = now - self.sim_at;
        if !dt.is_finite() || (dt > SNAP_AFTER && self.is_moving()) {
            self.reset(rect, now);
            return;
        }
        let _ = self.advance(now);
        let (cx, cy, w, h) = target_values(rect);
        self.center_x.target = cx;
        self.center_y.target = cy;
        self.width.target = w;
        self.height.target = h;
        self.target = Some(rect);
        self.slot_changed = slot_changed;
        self.snap_settled();
    }

    /// 推进到 `now`;返回这一帧的呈现矩形是否真的变了。
    pub fn advance(&mut self, now: f64) -> bool {
        if self.target.is_none() {
            self.sim_at = now;
            return false;
        }
        let dt = now - self.sim_at;
        if !dt.is_finite() || dt <= 0.0 {
            self.sim_at = now;
            return false;
        }
        self.sim_at = now;
        if !self.is_moving() {
            return false;
        }
        if dt > SNAP_AFTER {
            self.settle();
            return true;
        }
        let Some(before) = self.presented() else {
            return false;
        };
        let mut left = dt as f32;
        while left > 0.0 {
            let h = left.min(FULL_STEP);
            self.step(h);
            left -= h;
        }
        // 用 presented() 而非 presented_rect():最后一帧从重构矩形跳到
        // 精确 target 时也必须返回 changed,不能因为四舍五入看起来没变
        // 而漏提交。
        self.presented().is_some_and(|after| after != before)
    }

    /// 当前呈现的胶囊矩形;没有选中时为空。静止时直接返回原始目标,
    /// 不用 center/宽高重构,避免分数坐标在 f32 上来回换算丢位。
    pub fn presented(&self) -> Option<Rect> {
        let target = self.target?;
        if self.is_moving() {
            Some(self.presented_rect())
        } else {
            Some(target)
        }
    }

    /// 是否还有位置、尺寸或形变会在下一帧变化。
    pub fn is_moving(&self) -> bool {
        self.target.is_some()
            && (!self.center_x.settled()
                || !self.center_y.settled()
                || !self.width.settled()
                || !self.height.settled()
                || self.strain.value.abs() > STRAIN_EPS
                || self.strain.velocity.abs() > STRAIN_VEL_EPS
                || self.strain.target.abs() > STRAIN_EPS)
    }

    /// 配置关闭动效、系统减动效或高对比切换时,立即终止在途运动。
    pub fn settle(&mut self) {
        if self.target.is_none() {
            return;
        }
        self.center_x.settle();
        self.center_y.settle();
        self.width.settle();
        self.height.settle();
        self.strain.value = 0.0;
        self.strain.velocity = 0.0;
        self.strain.target = 0.0;
    }

    /// 没有选中:清空目标与全部速度,下一次组合从静止开始。
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn presented_rect(&self) -> Rect {
        let strain = self
            .strain
            .value
            .clamp(-STRAIN_HARD_LIMIT, STRAIN_HARD_LIMIT);
        let w = self.width.value.max(0.5) * strain.exp();
        let h = self.height.value.max(0.5) * (-strain).exp();
        Rect::from_xywh(
            self.center_x.value - w * 0.5,
            self.center_y.value - h * 0.5,
            w,
            h,
        )
    }

    fn step(&mut self, h: f32) {
        self.center_x.step(CENTER_K, CENTER_C, h);
        self.center_y.step(CENTER_K, CENTER_C, h);
        self.width.step(SIZE_K, SIZE_C, h);
        self.height.step(SIZE_K, SIZE_C, h);

        // 两轴各自的沿行进加速度取正交差:x 前进让 s>0(拉宽压高),
        // y 前进让 s<0(拉高压宽),斜向运动只看两轴净值。同一候选槽
        // 的尺寸重算不激励形变,旧应变只按自己的弹簧衰减。
        let short = self.width.target.min(self.height.target).max(1.0);
        let drive_x = drive_strain(&self.center_x, short);
        let drive_y = drive_strain(&self.center_y, short);
        let scale = STRAIN_GAIN / (short * STRAIN_K);
        self.strain.target = if self.slot_changed {
            ((drive_x - drive_y) * scale).clamp(-STRAIN_MAX, STRAIN_MAX)
        } else {
            0.0
        };
        self.strain.step(STRAIN_K, STRAIN_C, h);

        if !self.strain.value.is_finite() || !self.strain.velocity.is_finite() {
            self.strain.value = 0.0;
            self.strain.velocity = 0.0;
            self.strain.target = 0.0;
        }
        if self.strain.value.abs() > STRAIN_HARD_LIMIT {
            self.strain.value = self
                .strain
                .value
                .clamp(-STRAIN_HARD_LIMIT, STRAIN_HARD_LIMIT);
            self.strain.velocity *= 0.5;
        }
        self.snap_settled();
    }

    fn snap_settled(&mut self) {
        self.center_x.snap_if_close();
        self.center_y.snap_if_close();
        self.width.snap_if_close();
        self.height.snap_if_close();
        let base_settled = self.center_x.settled()
            && self.center_y.settled()
            && self.width.settled()
            && self.height.settled();
        if base_settled
            && self.strain.value.abs() <= STRAIN_EPS
            && self.strain.velocity.abs() <= STRAIN_VEL_EPS
        {
            self.strain.value = 0.0;
            self.strain.velocity = 0.0;
            self.strain.target = 0.0;
        }
    }
}

fn target_values(rect: Rect) -> (f32, f32, f32, f32) {
    let x = if rect.center_x().is_finite() {
        rect.center_x()
    } else {
        0.0
    };
    let y = if rect.center_y().is_finite() {
        rect.center_y()
    } else {
        0.0
    };
    let w = if rect.width().is_finite() {
        rect.width().max(1.0)
    } else {
        1.0
    };
    let h = if rect.height().is_finite() {
        rect.height().max(1.0)
    } else {
        1.0
    };
    (x, y, w, h)
}

fn drive_strain(spring: &Spring, short: f32) -> f32 {
    let v = spring.velocity;
    let floor = short * STRAIN_FLOOR_SPEED;
    let accel = spring.accel(CENTER_K, CENTER_C);
    accel * v / (v * v + floor * floor).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_xywh(x, y, w, h)
    }

    #[derive(Clone, Copy)]
    struct Sample {
        t: f64,
        w: f32,
        h: f32,
        v: f32,
        a: f32,
    }

    /// 采样一条运动轨迹;`y_axis` 选竖向时读 y 的中心弹簧。
    fn collect(m: &mut SelectionMotion, y_axis: bool, limit: f64) -> Vec<Sample> {
        let started = m.sim_at;
        let mut t = started;
        let mut out = Vec::new();
        while m.is_moving() && t < started + limit {
            t += 1.0 / 240.0;
            m.advance(t);
            let p = m.presented().unwrap();
            let (v, a) = if y_axis {
                (m.center_y.velocity, m.center_y.accel(CENTER_K, CENTER_C))
            } else {
                (m.center_x.velocity, m.center_x.accel(CENTER_K, CENTER_C))
            };
            out.push(Sample {
                t,
                w: p.width(),
                h: p.height(),
                v,
                a,
            });
        }
        out
    }

    #[test]
    fn rest_is_exact_target() {
        let target = rect(12.0, 18.0, 120.0, 34.0);
        let mut m = SelectionMotion::default();
        m.reset(target, 0.0);
        assert!(!m.is_moving());
        assert_eq!(m.presented(), Some(target));
        assert!(!m.advance(0.5));
        assert_eq!(m.presented(), Some(target));
    }

    #[test]
    fn horizontal_start_stretches_width_and_brake_squeezes_width() {
        let start = rect(0.0, 0.0, 120.0, 34.0);
        let end = rect(70.0, 0.0, 120.0, 34.0);
        let mut m = SelectionMotion::default();
        m.reset(start, 0.0);
        m.retarget(end, 0.0, true);

        let frames = collect(&mut m, false, 2.0);
        assert!(frames.len() > 2);
        assert_eq!(m.presented(), Some(end));
        for f in &frames {
            assert!(
                (f.w * f.h - 120.0 * 34.0).abs() < 0.5,
                "reciprocal bbox area should stay close: {}x{}",
                f.w,
                f.h
            );
        }
        // 按加速度相位找样本:启动帧速度向前且加速度为正;刹车帧速度
        // 仍向前但加速度已为负。形变弹簧滞后,刹车样本取区间末段。
        let startup = frames
            .iter()
            .rfind(|f| f.v > 10.0 && f.a > 0.0)
            .expect("must have an accelerating startup sample");
        let brake = frames
            .iter()
            .rfind(|f| f.v > 10.0 && f.a < 0.0)
            .expect("must have a braking sample still moving forward");
        assert!(startup.t < brake.t, "startup must precede braking");
        // X 启动:宽增、高减;X 刹车:宽减、高增。
        assert!(
            startup.w > 120.0 && startup.h < 34.0,
            "X startup must widen and flatten: {:.2}x{:.2}",
            startup.w,
            startup.h
        );
        assert!(
            brake.w < 120.0 && brake.h > 34.0,
            "X braking must narrow and heighten: {:.2}x{:.2}",
            brake.w,
            brake.h
        );
    }

    #[test]
    fn vertical_start_stretches_height_and_brake_squeezes_height() {
        let start = rect(0.0, 0.0, 34.0, 120.0);
        let end = rect(0.0, 70.0, 34.0, 120.0);
        let mut m = SelectionMotion::default();
        m.reset(start, 0.0);
        m.retarget(end, 0.0, true);

        let frames = collect(&mut m, true, 2.0);
        assert!(frames.len() > 2);
        assert_eq!(m.presented(), Some(end));
        for f in &frames {
            assert!(
                (f.w * f.h - 34.0 * 120.0).abs() < 0.5,
                "reciprocal bbox area should stay close: {}x{}",
                f.w,
                f.h
            );
        }
        let startup = frames
            .iter()
            .rfind(|f| f.v > 10.0 && f.a > 0.0)
            .expect("must have an accelerating startup sample");
        let brake = frames
            .iter()
            .rfind(|f| f.v > 10.0 && f.a < 0.0)
            .expect("must have a braking sample still moving forward");
        assert!(startup.t < brake.t, "startup must precede braking");
        // Y 启动:高增、宽减;Y 刹车:高减、宽增。
        assert!(
            startup.h > 120.0 && startup.w < 34.0,
            "Y startup must heighten and narrow: {:.2}x{:.2}",
            startup.w,
            startup.h
        );
        assert!(
            brake.h < 120.0 && brake.w > 34.0,
            "Y braking must shorten and widen: {:.2}x{:.2}",
            brake.w,
            brake.h
        );
    }

    #[test]
    fn reverse_retarget_keeps_position_and_velocity_then_brakes() {
        let mut m = SelectionMotion::default();
        m.reset(rect(0.0, 0.0, 120.0, 34.0), 0.0);
        m.retarget(rect(80.0, 0.0, 120.0, 34.0), 0.0, true);

        // 事件在 40ms 才到:retarget 必须先把在途状态推进到事件帧,
        // 再从那一帧换目标;不能把 40ms 前的旧位置当成起点。
        let mut reference = m.clone();
        reference.advance(0.04);
        let expected = reference.presented().unwrap();
        let x0 = expected.center_x();
        let v0 = reference.center_x.velocity;
        let strain0 = reference.strain.value;
        assert!(v0 > 50.0, "must be moving before reversing: {v0}");

        // 反向目标在当前位置之后;retarget 不能清位置/速度。
        m.retarget(rect(-100.0, 0.0, 120.0, 34.0), 0.04, true);
        let after = m.presented().unwrap();
        assert_eq!(after, expected);
        assert!((m.center_x.velocity - v0).abs() < 1e-5);
        assert!((m.strain.value - strain0).abs() < 1e-5);

        // 下一帧先刹车:速度下降,但惯性还让中心向前走一点。
        m.advance(0.04 + 1.0 / 240.0);
        let next = m.presented().unwrap();
        assert!(
            m.center_x.velocity < v0 - 50.0,
            "first frame after reverse must brake: {} -> {}",
            v0,
            m.center_x.velocity
        );
        assert!(m.center_x.velocity > 0.0);
        assert!(next.center_x() > x0);
    }

    #[test]
    fn left_anchored_width_change_does_not_excite_strain() {
        let mut m = SelectionMotion::default();
        m.reset(rect(20.0, 50.0, 100.0, 34.0), 0.0);
        // 真实文字宽度替换:左边锚住,中心随宽度移动,不是两边对称。
        m.retarget(rect(20.0, 50.0, 150.0, 34.0), 0.0, false);
        let mut t = 0.0;
        while m.is_moving() && t < 2.0 {
            t += 1.0 / 240.0;
            m.advance(t);
            let p = m.presented().unwrap();
            assert!((p.center_y() - 67.0).abs() < 1e-6);
            assert!(
                m.strain.value.abs() < 1e-6,
                "same-slot width change must not excite strain: {}",
                m.strain.value
            );
        }
        assert_eq!(m.presented(), Some(rect(20.0, 50.0, 150.0, 34.0)));
        assert!(!m.is_moving());
    }

    #[test]
    fn same_slot_change_keeps_existing_strain_continuous() {
        let mut m = SelectionMotion::default();
        m.reset(rect(0.0, 0.0, 100.0, 34.0), 0.0);
        m.retarget(rect(70.0, 0.0, 100.0, 34.0), 0.0, true);
        m.advance(0.03);
        let before = m.strain.value;
        let velocity = m.center_x.velocity;
        assert!(before.abs() > 1e-4, "need live strain before slot change");

        // 同一候选槽的文字宽度重算:几何连续改,旧应变不能被清零或
        // 重新激励,只按自己的弹簧衰减;基本位置/速度也不清。
        m.retarget(rect(0.0, 0.0, 150.0, 34.0), 0.03, false);
        assert!((m.strain.value - before).abs() < 1e-6);
        assert_eq!(m.center_x.velocity, velocity);
        m.advance(0.03 + 1.0 / 240.0);
        assert!(m.strain.value.abs() < before.abs() + 0.01);
        assert!(m.strain.value.is_finite());

        let mut t = 0.03;
        while m.is_moving() && t < 2.0 {
            t += 1.0 / 240.0;
            m.advance(t);
        }
        assert_eq!(m.presented(), Some(rect(0.0, 0.0, 150.0, 34.0)));
        assert!(!m.is_moving());
    }

    #[test]
    fn rapid_resizes_converge_and_clear_leaves_no_residue() {
        let mut m = SelectionMotion::default();
        m.reset(rect(0.0, 0.0, 100.0, 34.0), 0.0);
        let targets = [
            rect(0.0, 0.0, 150.0, 34.0),
            rect(0.0, 0.0, 80.0, 34.0),
            rect(0.0, 0.0, 170.0, 34.0),
            rect(0.0, 0.0, 120.0, 34.0),
        ];
        let mut t = 0.0;
        for target in targets {
            m.retarget(target, t, false);
            for _ in 0..8 {
                t += 1.0 / 120.0;
                m.advance(t);
            }
        }
        // 长停帧直接落位,不补跳;停稳后与目标逐位一致。
        assert!(m.advance(t + 2.0));
        assert_eq!(m.presented(), Some(*targets.last().unwrap()));
        assert!(!m.is_moving());
        assert!(!m.advance(t + 2.5));

        m.clear();
        assert_eq!(m.presented(), None);
        assert!(!m.is_moving());
        assert!(!m.advance(t + 3.0));

        // 清空后重新出现:从静止落在新目标上,不继承上一轮速度。
        let fresh = rect(20.0, 10.0, 90.0, 30.0);
        m.reset(fresh, t + 3.0);
        assert_eq!(m.presented(), Some(fresh));
        assert!(!m.is_moving());
    }

    #[test]
    fn long_gap_lands_instead_of_catching_up_on_old_target() {
        let a = rect(0.0, 0.0, 100.0, 34.0);
        let b = rect(70.0, 0.0, 100.0, 34.0);
        let c = rect(140.0, 0.0, 100.0, 34.0);
        let mut m = SelectionMotion::default();
        m.reset(a, 0.0);
        m.retarget(b, 0.0, true);
        assert!(m.advance(0.01));
        assert!(m.is_moving());
        // 一次长停帧:直接落在旧目标上,不再模拟整段路径。
        assert!(m.advance(10.0));
        assert_eq!(m.presented(), Some(b));
        assert!(!m.is_moving());

        let mut m = SelectionMotion::default();
        m.reset(a, 0.0);
        m.retarget(b, 0.0, true);
        m.advance(0.01);
        // retarget 跨过长停帧:直接落到新目标,不先补到旧目标再追。
        m.retarget(c, 10.0, true);
        assert_eq!(m.presented(), Some(c));
        assert!(!m.is_moving());
        assert_eq!(m.center_x.velocity, 0.0);
    }

    #[test]
    fn idle_retarget_after_seconds_still_starts_from_old_target() {
        let a = rect(0.0, 0.0, 100.0, 34.0);
        let b = rect(80.0, 0.0, 100.0, 34.0);
        let mut m = SelectionMotion::default();
        m.reset(a, 0.0);
        // 静止一秒没有 poll 不是掉帧:目标换到 B,画面仍停在 A,
        // 下一帧从 A 的弹簧状态向 B 起步。
        m.retarget(b, 1.0, true);
        assert_eq!(m.presented(), Some(a));
        assert!(m.is_moving());

        m.advance(1.0 + 1.0 / 240.0);
        let p = m.presented().unwrap();
        assert!(p.center_x() > a.center_x());
        assert!(p.center_x() < b.center_x());

        let mut t = 1.0;
        while m.is_moving() && t < 4.0 {
            t += 1.0 / 240.0;
            m.advance(t);
        }
        assert_eq!(m.presented(), Some(b));
        assert!(!m.is_moving());
    }

    #[test]
    fn fractional_target_is_returned_exactly_at_rest() {
        let target = rect(12.3, 7.7, 93.1, 31.9);
        let mut m = SelectionMotion::default();
        m.reset(target, 0.0);
        assert_eq!(m.presented(), Some(target));

        let dest = rect(50.25, 20.125, 88.375, 32.75);
        m.retarget(dest, 0.0, true);
        let mut t = 0.0;
        while m.is_moving() && t < 2.0 {
            t += 1.0 / 240.0;
            m.advance(t);
        }
        assert_eq!(m.presented(), Some(dest));
        assert!(!m.is_moving());
    }

    #[test]
    fn extreme_targets_stay_finite_and_settle_exactly() {
        let mut m = SelectionMotion::default();
        m.reset(rect(0.0, 0.0, 100.0, 34.0), 0.0);
        let stops = [
            rect(900.0, -400.0, 260.0, 30.0),
            rect(-1200.0, 700.0, 60.0, 90.0),
            rect(3000.0, 0.0, 400.0, 24.0),
            rect(40.0, 20.0, 120.0, 34.0),
        ];
        let mut t = 0.0;
        for target in stops {
            m.retarget(target, t, true);
            for _ in 0..4 {
                t += 0.013;
                m.advance(t);
                let p = m.presented().unwrap();
                assert!(
                    p.left.is_finite()
                        && p.top.is_finite()
                        && p.width().is_finite()
                        && p.height().is_finite()
                        && p.width() > 0.0
                        && p.height() > 0.0,
                    "presented rect must stay finite: {p:?}"
                );
                assert!(m.strain.value.abs() <= STRAIN_HARD_LIMIT + 1e-4);
            }
        }
        assert!(m.advance(t + 2.0));
        assert_eq!(m.presented(), Some(*stops.last().unwrap()));
        assert!(!m.is_moving());
    }

    #[test]
    fn settle_is_immediate_and_stops_wake_loop() {
        let mut m = SelectionMotion::default();
        let a = rect(0.0, 0.0, 100.0, 34.0);
        let b = rect(60.0, 20.0, 100.0, 34.0);
        m.reset(a, 0.0);
        m.retarget(b, 0.0, true);
        m.advance(0.03);
        assert!(m.is_moving());
        m.settle();
        assert_eq!(m.presented(), Some(b));
        assert!(!m.is_moving());
        assert!(!m.advance(0.04));
    }
}
