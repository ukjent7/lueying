use super::*;

// 固定 03b3b5d 的物化恢复；只适配私有终末行参数和 Prefix 新字段。
#[inline(never)]
fn materialized_resume<'a>(
    run: &'a TailRun,
    taken: usize,
    model: &SentenceModel,
) -> (Prefix<'a>, f32) {
    let n_embd = model.config.n_embd;
    debug_assert!(taken > 0 && taken <= run.cumulative.len());
    if taken > run.hidden.len() {
        let terminal = run.terminal.get_or_init(|| {
            let mut states = model.run_tail_states(
                run.base + run.hidden.len(),
                &run.keys,
                &run.values,
                None,
                &[run.pending],
            );
            let width = (run.base + run.hidden.len()) * n_embd;
            TerminalState {
                keys: states
                    .keys
                    .iter_mut()
                    .map(|row| row.split_off(width))
                    .collect(),
                values: states
                    .values
                    .iter_mut()
                    .map(|row| row.split_off(width))
                    .collect(),
                hidden: states.hidden.pop().expect("末位置已有前向结果"),
            }
        });
        let append = |prefix: &[Vec<f32>], last: &[Vec<f32>]| {
            prefix
                .iter()
                .zip(last)
                .map(|(prefix, last)| {
                    let mut row = prefix.clone();
                    row.extend_from_slice(last);
                    row
                })
                .collect()
        };
        return (
            Prefix {
                length: run.base + taken,
                keys: Cow::Owned(append(&run.keys, &terminal.keys)),
                values: Cow::Owned(append(&run.values, &terminal.values)),
                terminal: None,
                last_hidden: terminal.hidden.clone(),
            },
            run.cumulative[taken - 1],
        );
    }
    (
        Prefix {
            length: run.base + taken,
            keys: Cow::Borrowed(&run.keys),
            values: Cow::Borrowed(&run.values),
            terminal: None,
            last_hidden: run.hidden[taken - 1].clone(),
        },
        run.cumulative[taken - 1],
    )
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

fn equal_runs(actual: &TailRun, expected: &TailRun) {
    assert_eq!(actual.base, expected.base);
    assert_eq!(actual.pending, expected.pending);
    assert_eq!(bits(&actual.cumulative), bits(&expected.cumulative));
    assert_eq!(actual.keys.len(), expected.keys.len());
    assert_eq!(actual.values.len(), expected.values.len());
    for (left, right) in actual.keys.iter().zip(&expected.keys) {
        assert_eq!(bits(left), bits(right));
        assert_eq!(left.capacity(), left.len(), "最终 key 保留了额外容量");
    }
    for (left, right) in actual.values.iter().zip(&expected.values) {
        assert_eq!(bits(left), bits(right));
        assert_eq!(left.capacity(), left.len(), "最终 value 保留了额外容量");
    }
    assert_eq!(actual.hidden.len(), expected.hidden.len());
    for (left, right) in actual.hidden.iter().zip(&expected.hidden) {
        assert_eq!(bits(&left.values), bits(&right.values));
    }
}

type Resume = for<'a> fn(&'a TailRun, usize, &SentenceModel) -> (Prefix<'a>, f32);

#[inline(never)]
fn borrowed_resume<'a>(run: &'a TailRun, taken: usize, model: &SentenceModel) -> (Prefix<'a>, f32) {
    run.resume(taken, model)
}

fn continue_run(resume: Resume, model: &SentenceModel, run: &TailRun, tail: &[u32]) -> TailRun {
    let taken = run.cumulative.len();
    let (prefix, carried) = resume(run, taken, model);
    let rest = model.run_tail(&prefix, tail);
    run.joined(taken, carried, rest)
}

fn check_continuations(model: &SentenceModel, prefix_rows: usize, original_rows: usize) {
    let prefix = model.run_prefix(&vec![BOS; prefix_rows]);
    let tokens: Vec<_> = (0..original_rows).map(|i| 3 + i as u32 % 5).collect();
    let run = model.run_tail(&prefix, &tokens);
    assert!(run.terminal.get().is_none());
    let (resumed, _) = run.resume(tokens.len(), model);
    assert_eq!(resumed.keys.as_ptr(), run.keys.as_ptr());
    assert_eq!(resumed.values.as_ptr(), run.values.as_ptr());
    assert!(std::ptr::eq(
        resumed.terminal.unwrap(),
        run.terminal.get().unwrap()
    ));
    let before = FORWARD_POSITIONS.with(std::cell::Cell::get);
    run.resume(tokens.len(), model);
    assert_eq!(FORWARD_POSITIONS.with(std::cell::Cell::get), before);
    for row in run
        .terminal
        .get()
        .unwrap()
        .keys
        .iter()
        .chain(&run.terminal.get().unwrap().values)
    {
        assert_eq!(row.len(), model.config.n_embd);
        assert_eq!(row.capacity(), model.config.n_embd);
    }
    for tail in [&[3][..], &[4, 5][..], &[5, 6, 7][..]] {
        let mut actual = continue_run(borrowed_resume, model, &run, tail);
        let mut expected = continue_run(materialized_resume, model, &run, tail);
        equal_runs(&actual, &expected);
        for _ in 0..2 {
            actual = continue_run(borrowed_resume, model, &actual, &[6, 4]);
            expected = continue_run(materialized_resume, model, &expected, &[6, 4]);
            equal_runs(&actual, &expected);
        }
    }
}

#[test]
fn segmented_terminal_prefix_preserves_complete_continuations() {
    for width in [4, 12, 64] {
        for heads in [1, 2] {
            for layers in [0, 1, 3] {
                for quantized in [false, true] {
                    let model =
                        super::prefix_final_row_tests::model(width, heads, layers, quantized);
                    for prefix in [1, 4, 12] {
                        for original in [1, 3, 8] {
                            check_continuations(&model, prefix, original);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn split_qkv_appends_the_terminal_row_before_new_rows() {
    for prefix in [&[][..], &[1.0, -0.0][..]] {
        for last in [None, Some((&[2.0, -0.0][..], &[3.0, -0.0][..]))] {
            for qkv in [&[][..], &[4.0, -0.0, 5.0, -0.0, 6.0, -0.0][..]] {
                let rows = usize::from(!qkv.is_empty());
                let (q, k, v) = split_qkv_with_suffix(qkv, rows, 2, prefix, prefix, last);
                let (last_k, last_v) = last.unwrap_or((&[], &[]));
                let new_k = if rows == 0 { &[][..] } else { &qkv[2..4] };
                let new_v = if rows == 0 { &[][..] } else { &qkv[4..6] };
                assert_eq!(bits(&q), bits(&qkv[..rows * 2]));
                assert_eq!(bits(&k), bits(&[prefix, last_k, new_k].concat()));
                assert_eq!(bits(&v), bits(&[prefix, last_v, new_v].concat()));
                assert_eq!(k.capacity(), k.len());
                assert_eq!(v.capacity(), v.len());
            }
        }
    }
}

fn elapsed(
    resume: Resume,
    model: &SentenceModel,
    run: &mut TailRun,
    tail: &[u32],
    cold: bool,
    repeats: usize,
) -> f64 {
    let start = std::time::Instant::now();
    for _ in 0..repeats {
        if cold {
            run.terminal.take();
        }
        std::hint::black_box(continue_run(resume, model, run, std::hint::black_box(tail)));
    }
    start.elapsed().as_secs_f64()
}

fn paired_shape(
    model: &SentenceModel,
    prefix: usize,
    original: usize,
    continued: usize,
    cold: bool,
    failures: &mut Vec<String>,
) {
    let prefix_state = model.run_prefix(&vec![BOS; prefix]);
    let mut run = model.run_tail(&prefix_state, &vec![3; original]);
    let tail = vec![4; continued];
    equal_runs(
        &continue_run(borrowed_resume, model, &run, &tail),
        &continue_run(materialized_resume, model, &run, &tail),
    );
    let mut repeats = 1;
    while elapsed(materialized_resume, model, &mut run, &tail, cold, repeats) < 0.010 {
        repeats *= 2;
    }
    let mut ratios = Vec::new();
    let mut old_times = Vec::new();
    let mut new_times = Vec::new();
    for pair in 0..21 {
        let (old, new) = if pair % 2 == 0 {
            let old = elapsed(materialized_resume, model, &mut run, &tail, cold, repeats);
            let new = elapsed(borrowed_resume, model, &mut run, &tail, cold, repeats);
            (old, new)
        } else {
            let new = elapsed(borrowed_resume, model, &mut run, &tail, cold, repeats);
            let old = elapsed(materialized_resume, model, &mut run, &tail, cold, repeats);
            (old, new)
        };
        ratios.push(new / old);
        old_times.push(old / repeats as f64 * 1e6);
        new_times.push(new / repeats as f64 * 1e6);
    }
    ratios.sort_by(f64::total_cmp);
    old_times.sort_by(f64::total_cmp);
    new_times.sort_by(f64::total_cmp);
    let quantized = matches!(model.token.inner, Tensor::Quantized { .. });
    let budget = if quantized && model.blocks.len() == 6 && !cold && continued == 1 {
        0.99
    } else {
        1.05
    };
    let description = format!("layers={} quantized={quantized} prefix={prefix} original={original} continued={continued} cold={cold} repeats={repeats} old_us={:.3} new_us={:.3} ratio={:.6} p10={:.6} p90={:.6} budget={budget:.2}",
        model.blocks.len(), old_times[10], new_times[10], ratios[10], ratios[2], ratios[18]);
    println!("{description}");
    if ratios[10] >= budget {
        failures.push(description);
    }
}

#[test]
#[ignore = "独占运行完整末位置续跑的配对性能门禁"]
fn complete_terminal_continuation_paired_budget() {
    let mut failures = Vec::new();
    for layers in [1, 6] {
        for quantized in [false, true] {
            let model = super::prefix_final_row_tests::model(192, 4, layers, quantized);
            for prefix in [1, 12] {
                for original in [1, 8] {
                    for continued in [1, 3] {
                        for cold in [false, true] {
                            paired_shape(&model, prefix, original, continued, cold, &mut failures);
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "完整末位置续跑预算失败：{failures:?}");
}

#[test]
#[ignore = "需要固定真实模型核对完整末位置续跑并独占计时"]
fn real_model_terminal_continuation_bits_and_budget() {
    let directory = std::env::var_os("MSIME_NEURAL_MODEL_DIR").expect("需要固定模型目录");
    let bytes =
        std::fs::read(std::path::Path::new(&directory).join("sentence-model.safetensors")).unwrap();
    let model = SentenceModel::load(&bytes).unwrap();
    let mut failures = Vec::new();
    for prefix in [1, 12] {
        for original in [1, 8] {
            check_continuations(&model, prefix, original);
            for continued in [1, 3] {
                for cold in [false, true] {
                    paired_shape(&model, prefix, original, continued, cold, &mut failures);
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "实际模型末位置续跑预算失败：{failures:?}"
    );
}
