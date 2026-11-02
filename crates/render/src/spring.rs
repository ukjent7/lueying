//! 弹簧:一个标量在阻尼谐振子下回到目标值。
//!
//! 窗口层用它做出现动画:窗口从落点下方弹回,落定后精确停在目标上。
//! 积分用半隐式欧拉,步长由调用方给,与实际帧率无关。

/// 位置与目标之差小于这个量就认为已经落定(零点几个像素,看不出来)。
const VALUE_EPSILON: f32 = 0.05;
/// 速度小于这个量就认为已经落定。
const VELOCITY_EPSILON: f32 = 0.2;

/// 阻尼弹簧:从当前值弹向目标值。
///
/// 刚度越大越快,阻尼越小越弹;`mass` 是惯性,一般保持 1。
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    stiffness: f32,
    damping: f32,
    mass: f32,
}

impl Spring {
    /// 新建弹簧;初始值与目标都是 0。
    pub fn new(stiffness: f32, damping: f32, mass: f32) -> Self {
        Self {
            value: 0.0,
            velocity: 0.0,
            target: 0.0,
            stiffness,
            damping,
            mass,
        }
    }

    /// 立刻定位到 `value`,不留速度。
    pub fn set(&mut self, value: f32) {
        self.value = value;
        self.velocity = 0.0;
    }

    /// 当前值。
    pub fn value(&self) -> f32 {
        self.value
    }

    /// 当前速度。
    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    /// 目标值。
    pub fn target(&self) -> f32 {
        self.target
    }

    /// 推进 `dt` 秒;返回 true 表示还在动。
    pub fn step(&mut self, dt: f32) -> bool {
        let offset = self.value - self.target;
        let accel = (-self.stiffness * offset - self.damping * self.velocity) / self.mass;
        self.velocity += accel * dt;
        self.value += self.velocity * dt;
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            false
        } else {
            true
        }
    }

    /// 是否已经停在目标上。
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() <= VALUE_EPSILON
            && self.velocity.abs() <= VELOCITY_EPSILON
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 出现动画用的参数:约半秒内落定,带一次轻微过冲。
    const STIFFNESS: f32 = 360.0;
    const DAMPING: f32 = 28.0;
    const STEP: f32 = 1.0 / 60.0;

    #[test]
    fn 收敛到目标() {
        let mut spring = Spring::new(STIFFNESS, DAMPING, 1.0);
        spring.set(-6.0);
        let mut steps = 0;
        while spring.step(STEP) {
            steps += 1;
            assert!(steps < 600, "弹簧不收敛");
        }
        assert!(steps < 120, "弹簧应当在两秒内落定,实际 {steps} 步");
        assert_eq!(spring.value(), 0.0);
        assert_eq!(spring.velocity(), 0.0);
        assert!(spring.settled());
    }

    #[test]
    fn 欠阻尼会过冲() {
        let mut spring = Spring::new(STIFFNESS, 12.0, 1.0);
        spring.set(-6.0);
        let mut peak = f32::MIN;
        for _ in 0..120 {
            spring.step(STEP);
            peak = peak.max(spring.value());
        }
        assert!(peak > 0.0, "应当越过目标,实际峰值 {peak}");
    }

    #[test]
    fn 定位清掉速度() {
        let mut spring = Spring::new(STIFFNESS, DAMPING, 1.0);
        spring.step(STEP);
        spring.set(3.0);
        assert_eq!(spring.value(), 3.0);
        assert_eq!(spring.velocity(), 0.0);
    }

    #[test]
    fn 静止时不再动() {
        let mut spring = Spring::new(STIFFNESS, DAMPING, 1.0);
        assert!(!spring.step(STEP));
        assert_eq!(spring.value(), 0.0);
    }
}
