use super::*;

// 固定 527d2fa 的完整前缀入口；本片未改变任何底层算术 helper。
#[inline(never)]
fn baseline_prefix(model: &SentenceModel, tokens: &[u32]) -> Prefix<'static> {
    let n_embd = model.config.n_embd;
    let mut x = model.embed(tokens, 0);
    let mut feed_forward = FeedForwardScratch::default();
    let mut keys = Vec::with_capacity(model.blocks.len());
    let mut values = Vec::with_capacity(model.blocks.len());
    for block in &model.blocks {
        let normed = layer_norm(&x, &block.ln1_weight, &block.ln1_bias, n_embd);
        let qkv = linear(
            &normed,
            &block.qkv_weight,
            &block.qkv_bias,
            n_embd,
            3 * n_embd,
        );
        let (q, k, v) = split_qkv(&qkv, tokens.len(), n_embd, &[], &[]);
        let attended = model.attention(&q, &k, &v, tokens.len(), 0);
        let projected = linear(
            &attended,
            &block.proj_weight,
            &block.proj_bias,
            n_embd,
            n_embd,
        );
        for (slot, value) in x.iter_mut().zip(projected) {
            *slot += value;
        }
        model.feed_forward(&mut x, block, &mut feed_forward);
        keys.push(k);
        values.push(v);
    }
    let last = (tokens.len() - 1) * n_embd;
    let last_hidden = Arc::new(HiddenState::new(layer_norm(
        &x[last..],
        &model.final_weight,
        &model.final_bias,
        n_embd,
    )));
    Prefix {
        length: tokens.len(),
        keys: Cow::Owned(keys),
        values: Cow::Owned(values),
        terminal: None,
        last_hidden,
    }
}

#[test]
fn prefix_computes_only_the_last_query_in_the_final_layer() {
    let model = std::sync::Arc::new(model(8, 2, 1, true));
    let tokens = [BOS, 3, 4, 5];
    ATTENTION_POSITIONS.with(|count| count.set(0));
    let prefix = model.run_prefix(&tokens);
    assert_eq!(ATTENTION_POSITIONS.with(std::cell::Cell::get), 1);
    assert_eq!(prefix.keys[0].len(), tokens.len() * model.config.n_embd);
    assert_eq!(prefix.values[0].len(), tokens.len() * model.config.n_embd);
}

pub(super) fn model(width: usize, heads: usize, layers: usize, quantized: bool) -> SentenceModel {
    let matrix = |rows: usize, inputs: usize, salt: usize| Matrix {
        inner: if quantized {
            Tensor::Quantized {
                values: (0..rows * inputs)
                    .map(|i| ((i * 7 + salt) % 23) as i8 - 11)
                    .collect(),
                scales: (0..rows).map(|i| (i % 7 + 1) as f32 / 128.0).collect(),
            }
        } else {
            Tensor::Float(
                (0..rows * inputs)
                    .map(|i| (((i * 7 + salt) % 23) as f32 - 11.0) / 64.0)
                    .collect(),
            )
        },
    };
    let vector = |count, salt| {
        (0..count)
            .map(|i| (((i * 3 + salt) % 19) as f32 - 9.0) / 32.0)
            .collect()
    };
    SentenceModel {
        config: Config {
            vocab: 8,
            n_layer: layers,
            n_head: heads,
            n_embd: width,
            context: 64,
        },
        index: HashMap::new(),
        token: matrix(8, width, 1),
        position: vector(64 * width, 2),
        blocks: (0..layers)
            .map(|layer| Block {
                ln1_weight: vector(width, layer + 1),
                ln1_bias: vector(width, layer + 2),
                qkv_weight: matrix(3 * width, width, layer + 3),
                qkv_bias: vector(3 * width, layer + 4),
                proj_weight: matrix(width, width, layer + 5),
                proj_bias: vector(width, layer + 6),
                ln2_weight: vector(width, layer + 7),
                ln2_bias: vector(width, layer + 8),
                fc_weight: matrix(4 * width, width, layer + 9),
                fc_bias: vector(4 * width, layer + 10),
                out_weight: matrix(width, 4 * width, layer + 11),
                out_bias: vector(width, layer + 12),
            })
            .collect(),
        final_weight: vector(width, 13),
        final_bias: vector(width, 14),
        attribution: String::new(),
        license: String::new(),
    }
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

fn equal_prefix(a: &Prefix<'_>, b: &Prefix<'_>) {
    assert_eq!(a.length, b.length);
    assert_eq!(a.keys.len(), b.keys.len());
    assert_eq!(a.values.len(), b.values.len());
    for (a, b) in a
        .keys
        .iter()
        .chain(a.values.iter())
        .zip(b.keys.iter().chain(b.values.iter()))
    {
        assert_eq!(bits(a), bits(b));
    }
    assert_eq!(bits(&a.last_hidden.values), bits(&b.last_hidden.values));
}

#[test]
fn final_prefix_row_preserves_all_kv_hidden_and_continuations() {
    for quantized in [false, true] {
        for (width, heads) in [(1, 1), (7, 1), (8, 2), (9, 3), (192, 4)] {
            for layers in [0, 1, 3] {
                let model = model(width, heads, layers, quantized);
                for rows in [1, 4, 12] {
                    let tokens: Vec<_> = (0..rows).map(|i| i as u32 % 8).collect();
                    let old = baseline_prefix(&model, &tokens);
                    ATTENTION_POSITIONS.with(|count| count.set(0));
                    let new = model.run_prefix(&tokens);
                    let expected = if layers == 0 {
                        0
                    } else {
                        (layers - 1) * rows + 1
                    };
                    assert_eq!(ATTENTION_POSITIONS.with(std::cell::Cell::get), expected);
                    equal_prefix(&old, &new);
                    assert_eq!(
                        bits(&model.distribution(&old.last_hidden.values)),
                        bits(&model.distribution(&new.last_hidden.values))
                    );
                    for tail in [&[3][..], &[3, 4, 5][..]] {
                        let a = model.run_tail(&old, tail);
                        let b = model.run_tail(&new, tail);
                        assert_eq!(bits(&a.cumulative), bits(&b.cumulative));
                        for (a, b) in a
                            .keys
                            .iter()
                            .chain(a.values.iter())
                            .zip(b.keys.iter().chain(b.values.iter()))
                        {
                            assert_eq!(bits(a), bits(b));
                        }
                        for (a, b) in a.hidden.iter().zip(&b.hidden) {
                            assert_eq!(bits(&a.values), bits(&b.values));
                        }
                    }
                }
            }
        }
    }
}

#[inline(never)]
fn production_prefix(model: &SentenceModel, tokens: &[u32]) -> Prefix<'static> {
    model.run_prefix(tokens)
}

type Run = fn(&SentenceModel, &[u32]) -> Prefix<'static>;
fn elapsed(run: Run, model: &SentenceModel, tokens: &[u32], repeats: usize) -> f64 {
    let started = std::time::Instant::now();
    for _ in 0..repeats {
        drop(std::hint::black_box(run(
            std::hint::black_box(model),
            std::hint::black_box(tokens),
        )));
    }
    started.elapsed().as_secs_f64()
}

fn paired_shape(model: &SentenceModel, tokens: &[u32], failures: &mut Vec<String>) {
    equal_prefix(
        &baseline_prefix(model, tokens),
        &production_prefix(model, tokens),
    );
    let mut repeats = 1;
    while elapsed(baseline_prefix, model, tokens, repeats) < 0.010 {
        repeats *= 2;
    }
    let mut ratios = Vec::new();
    let mut old_samples = Vec::new();
    let mut new_samples = Vec::new();
    for pair in 0..21 {
        let (old, new) = if pair % 2 == 0 {
            let old = elapsed(baseline_prefix, model, tokens, repeats);
            (old, elapsed(production_prefix, model, tokens, repeats))
        } else {
            let new = elapsed(production_prefix, model, tokens, repeats);
            (elapsed(baseline_prefix, model, tokens, repeats), new)
        };
        ratios.push(new / old);
        old_samples.push(old / repeats as f64 * 1e6);
        new_samples.push(new / repeats as f64 * 1e6);
    }
    ratios.sort_by(f64::total_cmp);
    old_samples.sort_by(f64::total_cmp);
    new_samples.sort_by(f64::total_cmp);
    let budget = if tokens.len() > 1 { 0.98 } else { 1.05 };
    let description = format!("layers={} rows={} quantized={} old_us={:.3} new_us={:.3} ratio={:.6} p10={:.6} p90={:.6} budget={budget:.2}",
        model.config.n_layer, tokens.len(), matches!(model.token.inner, Tensor::Quantized { .. }),
        old_samples[10], new_samples[10], ratios[10], ratios[2], ratios[18]);
    println!("{description}");
    if ratios[10] >= budget {
        failures.push(description);
    }
}

#[test]
#[ignore = "独占运行完整上文构建的配对性能门禁"]
fn complete_prefix_paired_budget() {
    let mut failures = Vec::new();
    for quantized in [false, true] {
        for layers in [1, 6] {
            let model = model(192, 4, layers, quantized);
            for rows in [1, 4, 12, 32] {
                let tokens: Vec<_> = (0..rows).map(|i| i as u32 % 8).collect();
                paired_shape(&model, &tokens, &mut failures);
            }
        }
    }
    assert!(failures.is_empty(), "完整上文构建预算失败：{failures:?}");
}

#[test]
#[ignore = "需要固定实际模型逐位核对长上文并独占计时"]
fn real_model_prefix_bits_and_budget() {
    let directory = std::env::var_os("MSIME_NEURAL_MODEL_DIR").expect("需要固定模型目录");
    let bytes =
        std::fs::read(std::path::Path::new(&directory).join("sentence-model.safetensors")).unwrap();
    let model = SentenceModel::load(&bytes).unwrap();
    let mut failures = Vec::new();
    for rows in [1, 4, 12, 32, 64] {
        let tokens: Vec<_> = (0..rows)
            .map(|i| i as u32 % model.config.vocab as u32)
            .collect();
        paired_shape(&model, &tokens, &mut failures);
        let a = baseline_prefix(&model, &tokens);
        let b = model.run_prefix(&tokens);
        assert_eq!(
            bits(&model.distribution(&a.last_hidden.values)),
            bits(&model.distribution(&b.last_hidden.values))
        );
        if rows < 64 {
            let tail = [3, 4, 5];
            assert_eq!(
                bits(&model.run_tail(&a, &tail).cumulative),
                bits(&model.run_tail(&b, &tail).cumulative)
            );
        }
    }
    assert!(
        failures.is_empty(),
        "实际模型上文构建预算失败：{failures:?}"
    );
}
