use crate::{Config, Matrix, SentenceModel, Tensor};
use std::hint::black_box;
use std::time::Instant;

// 固定 362bf56 的完整投影正文，只把方法接收者改为显式参数。
#[inline(never)]
fn reference(model: &SentenceModel, hidden: &[f32]) -> (Vec<f32>, f32) {
    crate::VOCABULARY_PROJECTIONS.with(|count| count.set(count.get() + 1));
    let n_embd = model.config.n_embd;
    let mut logits = Vec::with_capacity(model.config.vocab);
    let mut highest = f32::NEG_INFINITY;
    for row in 0..model.config.vocab {
        let logit = model.token.dot_row(hidden, row, n_embd);
        highest = highest.max(logit);
        logits.push(logit);
    }
    let sum: f32 = logits.iter().map(|logit| (logit - highest).exp()).sum();
    let offset = highest + sum.ln();
    (logits, offset)
}

#[inline(never)]
fn production(model: &SentenceModel, hidden: &[f32]) -> (Vec<f32>, f32) {
    model.project(hidden)
}

fn fixture(width: usize, vocab: usize, quantized: bool) -> (SentenceModel, Vec<f32>) {
    let weights = if quantized {
        Tensor::Quantized {
            values: (0..vocab * width)
                .map(|index| ((index * 29 + 7) % 255) as i16 as i8)
                .collect(),
            scales: (0..vocab)
                .map(|index| ((index % 11) as f32 + 1.0) / 128.0)
                .collect(),
        }
    } else {
        Tensor::Float(
            (0..vocab * width)
                .map(|index| ((index * 29 % 251) as f32 - 125.0) / 128.0)
                .collect(),
        )
    };
    let model = SentenceModel {
        config: Config {
            vocab,
            n_layer: 0,
            n_head: 1,
            n_embd: width,
            context: 1,
        },
        index: Default::default(),
        token: Matrix { inner: weights },
        position: vec![0.0; width],
        blocks: Vec::new(),
        final_weight: vec![1.0; width],
        final_bias: vec![0.0; width],
        attribution: String::new(),
        license: String::new(),
    };
    model.check_geometry().expect("合成投影模型几何正确");
    let hidden = (0..width)
        .map(|index| ((index * 13 % 37) as f32 - 18.0) / 17.0)
        .collect();
    (model, hidden)
}

fn same_projection(model: &SentenceModel, hidden: &[f32]) {
    let expected = reference(model, hidden);
    let actual = production(model, hidden);
    assert_eq!(actual.0.len(), expected.0.len());
    for (actual, expected) in actual.0.iter().zip(&expected.0) {
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "词表 logit 位模式变化"
        );
    }
    assert_eq!(actual.1.to_bits(), expected.1.to_bits(), "归一化位模式变化");
}

#[test]
fn projection_preserves_every_logit_and_the_normalizer() {
    for width in [1, 7, 8, 9, 192, 768] {
        for vocab in [3, 9, 257] {
            for quantized in [false, true] {
                let (model, hidden) = fixture(width, vocab, quantized);
                same_projection(&model, &hidden);
            }
        }
    }
}

type Project = fn(&SentenceModel, &[f32]) -> (Vec<f32>, f32);

fn elapsed(project: Project, model: &SentenceModel, hidden: &[f32]) -> f64 {
    let started = Instant::now();
    for _ in 0..50 {
        black_box(project(black_box(model), black_box(hidden)));
    }
    started.elapsed().as_secs_f64()
}

#[test]
#[ignore = "手动配对完整词表投影，不与编译或其它计时并行"]
fn full_projection_paired_budget() {
    let mut failures = Vec::new();
    println!("quantized width vocab paired_ratio_p50");
    for (width, vocab, quantized) in [
        (192, 8192, true),
        (192, 8192, false),
        (768, 8192, true),
        (768, 8192, false),
    ] {
        let (model, hidden) = fixture(width, vocab, quantized);
        same_projection(&model, &hidden);
        let mut ratios = Vec::new();
        for pair in 0..40 {
            let (old, new) = if pair % 2 == 0 {
                let old = elapsed(reference, &model, &hidden);
                (old, elapsed(production, &model, &hidden))
            } else {
                let new = elapsed(production, &model, &hidden);
                (elapsed(reference, &model, &hidden), new)
            };
            ratios.push(new / old);
        }
        ratios.sort_by(f64::total_cmp);
        let ratio = ratios[19];
        println!("{quantized} {width} {vocab} {ratio:.4}");
        let budget = if quantized && width == 192 {
            0.99
        } else {
            1.03
        };
        if ratio >= budget {
            failures.push((width, vocab, quantized, ratio, budget));
        }
    }
    assert!(failures.is_empty(), "完整投影配对预算失败: {failures:?}");
}
