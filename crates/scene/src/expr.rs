//! 有界表达式:算术、比较与一组纯函数;无循环、无递归、无字符串运算。
//!
//! 场景文件里所有动态数值都是这里的表达式,例如 `opacity =
//! "lerp(0.15, 0.6, energy)"`。名字解析成数值走调用方给的名字表;求值
//! 带操作预算,超限返回 [`EvalError::Budget`],由运行时熔断整帧。随机数
//! 是种子进表达式的整数混淆,同输入必同输出,黄金测试才立得住。

/// 求值时的名字表;查不到的名字在加载期就该被拒。
pub type Names<'a> = &'a dyn Fn(&str) -> Option<f64>;

/// 求值失败原因;加载期已挡掉坏表达式,运行期只剩预算这一种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalError {
    /// 操作预算用尽:整帧熔断,后续表达式按 0 处理。
    Budget,
}

/// 一元运算:取负。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unary {
    Neg,
}

/// 二元运算;比较结果为 0/1,非零且非 NaN 视为真。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// 内置函数;求值会拿到场景种子(`rand` 用),其余函数忽略它。
pub struct Func {
    pub name: &'static str,
    pub arity: usize,
    pub f: fn(&[f64], u64) -> f64,
}

pub static FUNCS: &[Func] = &[
    Func {
        name: "lerp",
        arity: 3,
        f: lerp,
    },
    Func {
        name: "clamp",
        arity: 3,
        f: clamp,
    },
    Func {
        name: "min",
        arity: 2,
        f: |a, _| a[0].min(a[1]),
    },
    Func {
        name: "max",
        arity: 2,
        f: |a, _| a[0].max(a[1]),
    },
    Func {
        name: "abs",
        arity: 1,
        f: |a, _| a[0].abs(),
    },
    Func {
        name: "floor",
        arity: 1,
        f: |a, _| a[0].floor(),
    },
    Func {
        name: "ceil",
        arity: 1,
        f: |a, _| a[0].ceil(),
    },
    Func {
        name: "round",
        arity: 1,
        f: |a, _| a[0].round(),
    },
    Func {
        name: "sqrt",
        arity: 1,
        f: |a, _| a[0].sqrt(),
    },
    Func {
        name: "rand",
        arity: 1,
        f: rand,
    },
    // 缓动;输入与输出都约定在 0..1。
    Func {
        name: "linear",
        arity: 1,
        f: |a, _| a[0],
    },
    Func {
        name: "smooth",
        arity: 1,
        f: |a, _| smooth(a[0]),
    },
    Func {
        name: "out_quad",
        arity: 1,
        f: |a, _| 1.0 - (1.0 - a[0]) * (1.0 - a[0]),
    },
    Func {
        name: "out_cubic",
        arity: 1,
        f: |a, _| 1.0 - (1.0 - a[0]).powi(3),
    },
    Func {
        name: "out_quint",
        arity: 1,
        f: |a, _| 1.0 - (1.0 - a[0]).powi(5),
    },
    Func {
        name: "out_back",
        arity: 1,
        f: |a, _| {
            let c = 1.701_58;
            let t = a[0] - 1.0;
            1.0 + (c + 1.0) * t.powi(3) + c * t.powi(2)
        },
    },
];

fn lerp(a: &[f64], _: u64) -> f64 {
    a[0] + (a[1] - a[0]) * a[2]
}

fn clamp(a: &[f64], _: u64) -> f64 {
    a[0].clamp(a[1], a[2])
}

fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// 种子随机:SplitMix64 混淆输入位型,输出 0..1。跨平台逐位一致;
/// 表达式与粒子模拟共用这一份。
pub fn seeded(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let bits = z ^ (z >> 31);
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

fn rand(a: &[f64], seed: u64) -> f64 {
    seeded(&mut (seed ^ a[0].to_bits()))
}

/// 表达式语法树;只有数值一种类型。
#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Var(String),
    Call(usize, Vec<Expr>),
    Neg(Box<Expr>),
    Binary(Binary, Box<Expr>, Box<Expr>),
}

/// 解析失败;`pos` 是出错处距离表达式开头的字符数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub pos: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "第 {} 个字符附近:{}", self.pos + 1, self.message)
    }
}

const MAX_DEPTH: usize = 24;
const MAX_TOKENS: usize = 512;

/// 解析表达式;空串是错误(字段要么不写,要么写完整)。
pub fn parse(text: &str) -> Result<Expr, ParseError> {
    let tokens = lex(text)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        depth: 0,
        end: text.len(),
    };
    let expr = parser.expression()?;
    if parser.pos != tokens.len() {
        return Err(parser.fail("表达式后面还有多余的内容"));
    }
    Ok(expr)
}

#[derive(Debug, Clone)]
enum Tok {
    Num(f64),
    Name(String),
    Op(Binary),
    Neg,
    LParen,
    RParen,
    Comma,
}

fn lex(text: &str) -> Result<Vec<(usize, Tok)>, ParseError> {
    let mut tokens: Vec<(usize, Tok)> = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut index = 0;
    while index < chars.len() {
        let (pos, ch) = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        // 减号在数、名字、右括号之后是减法,否则是取负。
        let after_value = matches!(
            tokens.last().map(|(_, tok)| tok),
            Some(Tok::Num(_) | Tok::Name(_) | Tok::RParen)
        );
        let token = match ch {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ',' => Tok::Comma,
            '-' if after_value => Tok::Op(Binary::Sub),
            '-' => Tok::Neg,
            '+' => Tok::Op(Binary::Add),
            '*' => Tok::Op(Binary::Mul),
            '/' => Tok::Op(Binary::Div),
            '<' => {
                if chars.get(index + 1).is_some_and(|&(_, c)| c == '=') {
                    index += 1;
                    Tok::Op(Binary::Le)
                } else {
                    Tok::Op(Binary::Lt)
                }
            }
            '>' => {
                if chars.get(index + 1).is_some_and(|&(_, c)| c == '=') {
                    index += 1;
                    Tok::Op(Binary::Ge)
                } else {
                    Tok::Op(Binary::Gt)
                }
            }
            '=' => {
                if chars.get(index + 1).is_some_and(|&(_, c)| c == '=') {
                    index += 1;
                    Tok::Op(Binary::Eq)
                } else {
                    return Err(ParseError {
                        pos,
                        message: "单个 = 不是运算符,相等比较要写 ==".into(),
                    });
                }
            }
            '!' => {
                if chars.get(index + 1).is_some_and(|&(_, c)| c == '=') {
                    index += 1;
                    Tok::Op(Binary::Ne)
                } else {
                    return Err(ParseError {
                        pos,
                        message: "单个 ! 不是运算符".into(),
                    });
                }
            }
            digit if digit.is_ascii_digit() || digit == '.' => {
                let start = index;
                while index < chars.len() && chars[index].1.is_ascii_digit() {
                    index += 1;
                }
                if index < chars.len() && chars[index].1 == '.' {
                    index += 1;
                    while index < chars.len() && chars[index].1.is_ascii_digit() {
                        index += 1;
                    }
                }
                let literal: String = chars[start..index].iter().map(|&(_, c)| c).collect();
                let value: f64 = literal.parse().map_err(|_| ParseError {
                    pos,
                    message: format!("不是合法的数:{literal:?}"),
                })?;
                tokens.push((pos, Tok::Num(value)));
                continue;
            }
            letter if letter.is_ascii_alphabetic() || letter == '_' => {
                let start = index;
                while index < chars.len()
                    && (chars[index].1.is_ascii_alphanumeric() || chars[index].1 == '_')
                {
                    index += 1;
                }
                let name: String = chars[start..index].iter().map(|&(_, c)| c).collect();
                tokens.push((pos, Tok::Name(name)));
                continue;
            }
            other => {
                return Err(ParseError {
                    pos,
                    message: format!("不认识的字符 {other:?}"),
                });
            }
        };
        tokens.push((pos, token));
        if tokens.len() > MAX_TOKENS {
            return Err(ParseError {
                pos,
                message: format!("表达式超过 {MAX_TOKENS} 个记号,太长了"),
            });
        }
        index += 1;
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [(usize, Tok)],
    pos: usize,
    depth: usize,
    /// 表达式总长;结尾错误的报告位置。
    end: usize,
}

impl Parser<'_> {
    fn fail(&self, message: impl Into<String>) -> ParseError {
        let pos = self.tokens.get(self.pos).map_or(self.end, |&(pos, _)| pos);
        ParseError {
            pos,
            message: message.into(),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|(_, tok)| tok)
    }

    /// 比较;加减乘除优先级更高,比较不连写(`a < b < c` 是错误)。
    fn expression(&mut self) -> Result<Expr, ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.fail(format!("嵌套超过 {MAX_DEPTH} 层")));
        }
        let left = self.sum()?;
        let op = match self.peek() {
            Some(Tok::Op(
                op @ (Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge | Binary::Eq | Binary::Ne),
            )) => *op,
            _ => {
                self.depth -= 1;
                return Ok(left);
            }
        };
        self.pos += 1;
        let right = self.sum()?;
        self.depth -= 1;
        Ok(Expr::Binary(op, Box::new(left), Box::new(right)))
    }

    fn sum(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.product()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(Binary::Add)) => Binary::Add,
                Some(Tok::Op(Binary::Sub)) => Binary::Sub,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.product()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    fn product(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(Binary::Mul)) => Binary::Mul,
                Some(Tok::Op(Binary::Div)) => Binary::Div,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.unary()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if let Some(Tok::Neg) = self.peek() {
            self.pos += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let Some((_, token)) = self.tokens.get(self.pos) else {
            return Err(self.fail("表达式到这里就断了"));
        };
        match token.clone() {
            Tok::Num(value) => {
                self.pos += 1;
                Ok(Expr::Num(value))
            }
            Tok::Name(name) => {
                self.pos += 1;
                if !matches!(self.peek(), Some(Tok::LParen)) {
                    return Ok(Expr::Var(name));
                }
                self.pos += 1;
                let index = FUNCS
                    .iter()
                    .position(|func| func.name == name)
                    .ok_or_else(|| self.fail(format!("没有函数 {name:?}")))?;
                let mut args = Vec::new();
                if !matches!(self.peek(), Some(Tok::RParen)) {
                    loop {
                        args.push(self.expression()?);
                        match self.peek() {
                            Some(Tok::Comma) => self.pos += 1,
                            Some(Tok::RParen) => break,
                            _ => return Err(self.fail("参数列表缺右括号")),
                        }
                    }
                }
                self.pos += 1;
                let arity = FUNCS[index].arity;
                if args.len() != arity {
                    return Err(
                        self.fail(format!("{name} 要 {arity} 个参数,给了 {} 个", args.len()))
                    );
                }
                Ok(Expr::Call(index, args))
            }
            Tok::LParen => {
                self.pos += 1;
                let inner = self.expression()?;
                if !matches!(self.peek(), Some(Tok::RParen)) {
                    return Err(self.fail("缺右括号"));
                }
                self.pos += 1;
                Ok(inner)
            }
            _ => Err(self.fail("这里应该是数、名字或括号")),
        }
    }
}

impl Expr {
    /// 求值;`budget` 是本帧剩余操作数,每算一个节点扣一,扣完报
    /// [`EvalError::Budget`]。名字查不到按 0(加载期已验证过,这里只是兜底)。
    pub fn eval(&self, names: Names<'_>, seed: u64, budget: &mut u32) -> Result<f64, EvalError> {
        if *budget == 0 {
            return Err(EvalError::Budget);
        }
        *budget -= 1;
        Ok(match self {
            Expr::Num(value) => *value,
            Expr::Var(name) => names(name).unwrap_or(0.0),
            Expr::Call(index, args) => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(arg.eval(names, seed, budget)?);
                }
                (FUNCS[*index].f)(&values, seed)
            }
            Expr::Neg(inner) => -inner.eval(names, seed, budget)?,
            Expr::Binary(op, left, right) => {
                let a = left.eval(names, seed, budget)?;
                let b = right.eval(names, seed, budget)?;
                match op {
                    Binary::Add => a + b,
                    Binary::Sub => a - b,
                    Binary::Mul => a * b,
                    Binary::Div => a / b,
                    Binary::Lt => f64::from((a < b) as u8),
                    Binary::Le => f64::from((a <= b) as u8),
                    Binary::Gt => f64::from((a > b) as u8),
                    Binary::Ge => f64::from((a >= b) as u8),
                    Binary::Eq => f64::from((a == b) as u8),
                    Binary::Ne => f64::from((a != b) as u8),
                }
            }
        })
    }

    /// 收集表达式引用的全部名字;加载期拿去和变量表对账。
    pub fn names(&self, out: &mut Vec<String>) {
        match self {
            Expr::Num(_) => {}
            Expr::Var(name) => out.push(name.clone()),
            Expr::Call(_, args) => args.iter().for_each(|arg| arg.names(out)),
            Expr::Neg(inner) => inner.names(out),
            Expr::Binary(_, left, right) => {
                left.names(out);
                right.names(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<f64> {
        match name {
            "energy" => Some(0.5),
            "rate" => Some(2.0),
            _ => None,
        }
    }

    fn eval(text: &str) -> f64 {
        parse(text)
            .expect(text)
            .eval(&vars, 0, &mut 1024)
            .expect("budget")
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval("1 + 2 * 3"), 7.0);
        assert_eq!(eval("(1 + 2) * 3"), 9.0);
        assert_eq!(eval("-2 + 1"), -1.0);
        assert_eq!(eval("2 * -3"), -6.0);
        assert_eq!(eval("7 / 2"), 3.5);
        assert_eq!(eval("5 - -2"), 7.0);
    }

    #[test]
    fn comparisons_are_zero_or_one() {
        assert_eq!(eval("1 < 2"), 1.0);
        assert_eq!(eval("2 <= 1"), 0.0);
        assert_eq!(eval("2 == 2"), 1.0);
        assert_eq!(eval("2 != 2"), 0.0);
        assert_eq!(eval("1 >= 1"), 1.0);
    }

    #[test]
    fn functions_and_vars() {
        assert_eq!(eval("lerp(0, 10, 0.25)"), 2.5);
        assert_eq!(eval("clamp(5, 0, 3)"), 3.0);
        assert_eq!(eval("lerp(0.15, 0.6, energy)"), 0.375);
        assert_eq!(eval("energy * rate"), 1.0);
        assert_eq!(eval("out_quint(1)"), 1.0);
    }

    #[test]
    fn rand_is_deterministic_and_bounded() {
        let a = eval("rand(1)");
        let b = eval("rand(1)");
        let c = eval("rand(2)");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!((0.0..1.0).contains(&a));
    }

    #[test]
    fn seed_changes_rand() {
        let text = parse("rand(1)").unwrap();
        let a = text.eval(&vars, 1, &mut 64).unwrap();
        let b = text.eval(&vars, 2, &mut 64).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn budget_stops_evaluation() {
        let expr = parse("1 + 1").unwrap();
        assert_eq!(expr.eval(&vars, 0, &mut 0), Err(EvalError::Budget));
    }

    #[test]
    fn parse_errors_carry_position() {
        // "1 +" 在第 3 个字符处断掉。
        assert_eq!(parse("1 +").unwrap_err().pos, 3);
        assert!(parse("1 + @").unwrap_err().message.contains("不认识的字符"));
        assert!(
            parse("lerp(1, 2)")
                .unwrap_err()
                .message
                .contains("3 个参数")
        );
        assert!(parse("nope(1)").unwrap_err().message.contains("没有函数"));
        assert!(parse("x = 1").unwrap_err().message.contains("=="));
    }

    #[test]
    fn collected_names() {
        let expr = parse("lerp(0, 1, energy) + other * 2").unwrap();
        let mut names = Vec::new();
        expr.names(&mut names);
        assert_eq!(names, ["energy", "other"]);
    }
}
