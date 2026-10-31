//! 模型与重排的单元测试。

use super::{comparable, erf, gelu, should_rerank, CandidateFacts, Compared};

// 一层小模型只使用合成权重，缓存测试不读取外部模型文件。
pub(crate) fn cache_test_model() -> std::sync::Arc<super::SentenceModel> {
    use super::{Block, Config, Matrix, SentenceModel};
    let matrix = |size| Matrix {
        inner: super::Tensor::Float((0..size).map(|i| ((i % 17) as f32 - 8.0) / 32.0).collect()),
    };
    std::sync::Arc::new(SentenceModel {
        config: Config {
            vocab: 8,
            n_layer: 1,
            n_head: 2,
            n_embd: 4,
            context: 8,
        },
        index: ['你', '好', '吗', '我', '很']
            .into_iter()
            .enumerate()
            .map(|(index, character)| (character, index as u32 + 3))
            .collect(),
        token: matrix(8 * 4),
        position: (0..8 * 4)
            .map(|i| (((i * 7) % 19) as f32 - 9.0) / 64.0)
            .collect(),
        blocks: vec![Block {
            ln1_weight: vec![1.0; 4],
            ln1_bias: vec![0.0; 4],
            qkv_weight: matrix(12 * 4),
            qkv_bias: vec![0.0; 12],
            proj_weight: matrix(4 * 4),
            proj_bias: vec![0.0; 4],
            ln2_weight: vec![1.0; 4],
            ln2_bias: vec![0.0; 4],
            fc_weight: matrix(16 * 4),
            fc_bias: vec![0.0; 16],
            out_weight: matrix(4 * 16),
            out_bias: vec![0.0; 4],
        }],
        final_weight: vec![1.0; 4],
        final_bias: vec![0.0; 4],
        attribution: String::new(),
        license: String::new(),
    })
}

#[test]
fn cold_batch_shares_forward_positions_and_completed_runs() {
    let mut reranker = super::Reranker::new(cache_test_model());
    let positions = || super::FORWARD_POSITIONS.with(std::cell::Cell::get);
    let projections = || super::VOCABULARY_PROJECTIONS.with(std::cell::Cell::get);
    let candidates = ["你好我", "你好很", "你好我", "你好我吗"];
    for context in ["我", "很"] {
        let before = (positions(), projections());
        let scores = reranker.log_probabilities(context, &candidates);
        assert_eq!(positions() - before.0, 5, "同批共享前缀仍重复前向");
        assert_eq!(projections() - before.1, 4, "同批共享状态仍重复投影");
        assert_eq!(scores[0].to_bits(), scores[2].to_bits());
        assert!(std::sync::Arc::ptr_eq(
            &reranker.resume[0].1,
            &reranker.resume[2].1
        ));
        for (_, run) in &reranker.resume[1..] {
            assert!(std::sync::Arc::ptr_eq(
                &reranker.resume[0].1.hidden[0],
                &run.hidden[0]
            ));
        }
        let retained = std::sync::Arc::downgrade(&reranker.resume[0].1);
        let before = positions();
        reranker.log_probabilities(context, &["你好我吗", "你好我", "你好很"]);
        assert_eq!(positions() - before, 0, "重排完全命中仍做前向");
        reranker.log_probabilities(context, &[]);
        assert!(retained.upgrade().is_none(), "本轮复用不应延长缓存寿命");
    }
}

#[test]
fn same_length_reuse_keeps_the_previous_round_tie_order() {
    let model = cache_test_model();
    let mut reranker = super::Reranker::new(model.clone());
    reranker.log_probabilities("我", &["你好", "你吗"]);
    // 构造有效但独立的旧轮状态，让地址能区分同长匹配的来源。
    let prefix = &reranker.cached.as_ref().unwrap().1;
    for (tokens, run) in &mut reranker.resume {
        *run = std::sync::Arc::new(model.run_tail(prefix, tokens));
    }
    let expected = reranker.resume[1].1.hidden[0].clone();
    reranker.log_probabilities("我", &["你很", "你我"]);
    for (_, run) in &reranker.resume {
        assert!(std::sync::Arc::ptr_eq(&expected, &run.hidden[0]));
    }
}

#[test]
fn terminal_forward_waits_for_a_shared_continuation() {
    let mut reranker = super::Reranker::new(cache_test_model());
    let count = || super::FORWARD_POSITIONS.with(std::cell::Cell::get);
    let before = count();
    reranker.log_probabilities("我", &["你好", "你吗"]);
    assert_eq!(
        count() - before,
        3,
        "上文和 BOS 加共享首字，候选末字无需前向"
    );

    let before = count();
    reranker.log_probabilities("我", &["你很", "你我"]);
    assert_eq!(count() - before, 0, "只改末字仍做了前向");

    let before = count();
    reranker.log_probabilities("我", &["你很好", "你很吗"]);
    assert_eq!(count() - before, 1, "同一末位置应只按需补算一次");

    let before = count();
    reranker.log_probabilities("我", &["你很好我", "你很好吗"]);
    assert_eq!(count() - before, 1, "拼接后的末位置仍应共享补算");

    let before = count();
    reranker.log_probabilities("我", &["你很好", "你", "你很好"]);
    assert_eq!(count() - before, 0, "缩短候选不应补算末位置");
}

#[test]
fn split_qkv_preserves_prefix_and_row_order() {
    let qkv = [
        1.0, -0.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
    ];
    let keys = [21.0, 22.0, 23.0, 24.0];
    let values = [31.0, 32.0, 33.0, 34.0];
    let bits = |row: &[f32]| row.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
    for (prefix_keys, prefix_values) in [(&[][..], &[][..]), (&keys[..], &values[..])] {
        let (q, k, v) = super::split_qkv(&qkv, 2, 2, prefix_keys, prefix_values);
        assert_eq!(bits(&q), bits(&[1.0, -0.0, 7.0, 8.0]));
        let expected_keys = [prefix_keys, &[3.0, 4.0, 9.0, 10.0]].concat();
        let expected_values = [prefix_values, &[5.0, 6.0, 11.0, 12.0]].concat();
        assert_eq!(bits(&k), bits(&expected_keys));
        assert_eq!(bits(&v), bits(&expected_values));
        let (q, k, v) = super::split_qkv(&[], 0, 2, prefix_keys, prefix_values);
        assert!(q.is_empty());
        assert_eq!(bits(&k), bits(prefix_keys));
        assert_eq!(bits(&v), bits(prefix_values));
    }
}

#[test]
fn a_short_tail_does_not_double_the_retained_kv_capacity() {
    let model = cache_test_model();
    let prefix = model.run_prefix(&[super::BOS, 3, 4, 5, 6]);
    let states = model.run_tail_states(5, &prefix.keys, &prefix.values, None, &[3]);
    let width = 6 * model.config.n_embd;
    for row in states.keys.iter().chain(&states.values) {
        assert_eq!(row.len(), width);
        assert_eq!(row.capacity(), width, "短尾部保留了几何扩容后的 KV 容量");
    }
}

#[test]
fn ordinary_resume_borrows_the_existing_kv_storage() {
    let model = cache_test_model();
    let prefix = model.run_prefix(&model.prefix_tokens("我", 4));
    let run = model.run_tail(&prefix, &model.encode("你好我很"));
    let (resumed, _) = run.resume(1, &model);
    assert_eq!(
        resumed.keys.as_ptr(),
        run.keys.as_ptr(),
        "普通续跑复制了 KV 外层数组"
    );
    for (left, right) in resumed.keys.iter().zip(&run.keys) {
        assert_eq!(left.as_ptr(), right.as_ptr(), "普通续跑复制了 key");
    }
    for (left, right) in resumed.values.iter().zip(&run.values) {
        assert_eq!(left.as_ptr(), right.as_ptr(), "普通续跑复制了 value");
    }
}

#[test]
fn terminal_resume_borrows_the_existing_kv_storage() {
    let model = cache_test_model();
    let prefix = model.run_prefix(&model.prefix_tokens("我", 4));
    for text in ["你", "你好我很"] {
        let tokens = model.encode(text);
        let run = model.run_tail(&prefix, &tokens);
        let (resumed, _) = run.resume(tokens.len(), &model);
        assert_eq!(
            resumed.keys.as_ptr(),
            run.keys.as_ptr(),
            "末位置续跑复制了 KV 外层数组"
        );
        assert_eq!(resumed.values.as_ptr(), run.values.as_ptr());
        for (left, right) in resumed.keys.iter().zip(&run.keys) {
            assert_eq!(left.as_ptr(), right.as_ptr(), "末位置续跑复制了 key");
        }
        for (left, right) in resumed.values.iter().zip(&run.values) {
            assert_eq!(left.as_ptr(), right.as_ptr(), "末位置续跑复制了 value");
        }
    }
}

#[test]
fn borrowed_resume_matches_a_truncated_owned_prefix() {
    let model = cache_test_model();
    let prefix = model.run_prefix(&model.prefix_tokens("我", 4));
    let run = model.run_tail(&prefix, &model.encode("你好我很"));
    for taken in 1..=run.cumulative.len() {
        let (resumed, carried) = run.resume(taken, &model);
        let width = resumed.length * model.config.n_embd;
        let owned = super::Prefix {
            length: resumed.length,
            keys: std::borrow::Cow::Owned(
                run.keys
                    .iter()
                    .enumerate()
                    .map(|(layer, row)| {
                        if taken > run.hidden.len() {
                            [row.as_slice(), &run.terminal.get().unwrap().keys[layer]].concat()
                        } else {
                            row[..width].to_vec()
                        }
                    })
                    .collect(),
            ),
            values: std::borrow::Cow::Owned(
                run.values
                    .iter()
                    .enumerate()
                    .map(|(layer, row)| {
                        if taken > run.hidden.len() {
                            [row.as_slice(), &run.terminal.get().unwrap().values[layer]].concat()
                        } else {
                            row[..width].to_vec()
                        }
                    })
                    .collect(),
            ),
            last_hidden: resumed.last_hidden.clone(),
            terminal: None,
        };
        assert_eq!(carried.to_bits(), run.cumulative[taken - 1].to_bits());
        for text in ["吗", "吗你", "吗你好"] {
            let tail = model.encode(text);
            let actual = model.run_tail(&resumed, &tail);
            let expected = model.run_tail(&owned, &tail);
            assert_eq!(actual.base, expected.base);
            assert_eq!(actual.pending, expected.pending);
            assert_eq!(actual.keys, expected.keys);
            assert_eq!(actual.values, expected.values);
            assert_eq!(actual.hidden.len(), expected.hidden.len());
            assert_eq!(
                actual
                    .cumulative
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                expected
                    .cumulative
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>()
            );
            for (left, right) in actual.hidden.iter().zip(&expected.hidden) {
                assert_eq!(left.values, right.values);
            }
        }
    }
}

#[test]
fn a_single_character_tail_can_be_extended_without_an_eager_state() {
    let model = cache_test_model();
    let mut reranker = super::Reranker::new(model.clone());
    let count = || super::FORWARD_POSITIONS.with(std::cell::Cell::get);
    let before = count();
    reranker.log_probabilities("我", &["你", "我"]);
    assert_eq!(count() - before, 2, "单字候选只需要上文前向");
    let before = count();
    let scores = reranker.log_probabilities("我", &["你好", "你吗"]);
    assert_eq!(count() - before, 1, "没有已计算候选位置时也应共享末字补算");
    let fresh = model.score("我", &["你好", "你吗"]);
    assert_eq!(scores, fresh);
}

#[test]
fn identical_candidates_keep_the_completed_run() {
    let mut reranker = super::Reranker::new(cache_test_model());
    let before = reranker.log_probabilities("我", &["你好", "你吗"]);
    let retained = std::sync::Arc::downgrade(&reranker.resume[0].1);
    let allocation = reranker.resume[0].1.cumulative.as_ptr();
    let hidden = reranker.resume[0].1.hidden[0].values.as_ptr();
    // 重排、增删和重复候选不能消耗其它候选还要借用的旧状态。
    let after = reranker.log_probabilities("我", &["你吗我", "你好", "你好"]);
    assert_eq!(before[0].to_bits(), after[1].to_bits());
    assert_eq!(after[1].to_bits(), after[2].to_bits());
    for index in [1, 2] {
        assert_eq!(
            allocation,
            reranker.resume[index].1.cumulative.as_ptr(),
            "完全命中仍重新创建了累计分"
        );
        assert_eq!(
            hidden,
            reranker.resume[index].1.hidden[0].values.as_ptr(),
            "完全命中仍重新计算或复制了隐藏状态"
        );
    }
    assert_eq!(reranker.resume.len(), 3);
    reranker.log_probabilities("我", &["你好"]);
    assert_eq!(reranker.resume.len(), 1);
    reranker.log_probabilities("我", &[]);
    assert!(reranker.resume.is_empty());
    assert!(retained.upgrade().is_none());
}

#[test]
fn fresh_score_moves_encoded_tail_without_copying_it() {
    let mut reranker = super::Reranker::new(cache_test_model());
    super::LAST_ENCODED_POINTER.with(|pointer| pointer.set(0));
    reranker.log_probabilities("我", &["你好"]);
    let encoded = super::LAST_ENCODED_POINTER.with(std::cell::Cell::get);
    assert_eq!(
        encoded,
        reranker.resume[0].0.as_ptr() as usize,
        "首次评分仍复制了编码 token 缓冲"
    );
}

#[test]
fn reranker_reuses_the_resume_buffer_capacity() {
    let mut reranker = super::Reranker::new(cache_test_model());
    reranker.log_probabilities("我", &["你好", "你吗"]);
    reranker.log_probabilities("我", &["你好我", "你吗我"]);
    let spare = reranker.resume_scratch.as_ptr();
    assert!(reranker.resume_scratch.capacity() >= reranker.resume.len());
    assert!(reranker.resume_scratch.capacity() >= 2);
    reranker.log_probabilities("我", &["你好很", "你吗很"]);
    assert_eq!(reranker.resume.as_ptr(), spare);
}

#[test]
fn best_where_reuses_the_score_buffer_capacity() {
    let mut reranker = super::Reranker::new(cache_test_model());
    let facts = answering(&[true, true]);
    reranker.best_where("我", &["你好", "你吗"], &facts);
    let spare = reranker.scores_scratch.as_ptr();
    assert!(reranker.scores_scratch.capacity() >= 2);
    reranker.best_where("我", &["你很", "你我"], &facts);
    assert_eq!(reranker.scores_scratch.as_ptr(), spare);
}

#[test]
fn growing_candidate_lists_keep_score_storage_within_the_comparison_budget() {
    let mut reranker = super::Reranker::new(cache_test_model());
    let texts = ["你好"; 9];
    let facts = answering(&[true; 9]);
    for count in 2..=texts.len() {
        reranker.best_where("我", &texts[..count], &facts);
        assert!(
            reranker.scores_scratch.capacity() <= texts.len(),
            "{count} candidates retain capacity {}",
            reranker.scores_scratch.capacity()
        );
    }
}

#[test]
fn exact_reuse_moves_encoded_tail_without_copying_it() {
    let mut reranker = super::Reranker::new(cache_test_model());
    reranker.log_probabilities("我", &["你好"]);
    super::LAST_ENCODED_POINTER.with(|pointer| pointer.set(0));
    reranker.log_probabilities("我", &["你好"]);
    let encoded = super::LAST_ENCODED_POINTER.with(std::cell::Cell::get);
    assert_eq!(
        encoded,
        reranker.resume[0].0.as_ptr() as usize,
        "复用候选仍复制了编码 token 缓冲"
    );
}

#[test]
fn overlong_candidates_keep_only_the_model_window() {
    let mut reranker = super::Reranker::new(cache_test_model());
    for text in ["你", "A", "🎹"].map(|unit| unit.repeat(65_536)) {
        for _ in 0..2 {
            reranker.log_probabilities("我", &[&text]);
            let window = reranker.model.config.context - 1;
            assert_eq!(reranker.resume[0].0.len(), window);
            assert!(
                reranker.resume[0].0.capacity() <= 2 * window,
                "候选缓存仍保留窗口之外的编码容量：{}",
                reranker.resume[0].0.capacity()
            );
            let encoded = super::LAST_ENCODED_LENGTH.with(std::cell::Cell::get);
            assert_eq!(encoded, window, "仍编码了不会参与评分的尾部");
        }
    }
}

#[test]
fn shared_prefix_normalization_is_computed_once() {
    let mut reranker = super::Reranker::new(cache_test_model());
    let count = || super::VOCABULARY_PROJECTIONS.with(std::cell::Cell::get);
    let before = count();
    reranker.log_probabilities("我", &["你好", "你吗"]);
    // 上文和同批共享的首字状态各投影一次。
    assert_eq!(count() - before, 2, "同一上文或同批前缀重复投影词表");

    let before = count();
    reranker.log_probabilities("我", &["你好我", "你好很"]);
    assert_eq!(count() - before, 1, "同一续跑位置重复投影词表");

    let before = count();
    reranker.log_probabilities("我", &["你很", "你我"]);
    assert_eq!(count() - before, 0, "已知归一化常数的共享位置仍重复计算");

    let before = count();
    reranker.log_probabilities("很", &["你好", "你吗"]);
    assert_eq!(count() - before, 2, "上下文改变后必须使用新状态");
}

#[test]
fn cached_normalization_matches_full_distribution_bits() {
    let model = cache_test_model();
    for context in ["", "我", "你好我很"] {
        let prefix = model.run_prefix(&model.prefix_tokens(context, 4));
        let run = model.run_tail(&prefix, &[3, 4, 5, 6]);
        assert!(run.terminal.get().is_none(), "未续跑的末位置不应提前计算");
        for state in std::iter::once(&prefix.last_hidden).chain(&run.hidden) {
            let full = model.distribution(&state.values);
            for token in 0..model.config.vocab + 3 {
                let expected = full[token.min(model.config.vocab - 1)];
                assert_eq!(
                    model.log_probability(state, token as u32).to_bits(),
                    expected.to_bits(),
                    "单 token 概率与完整分布不同"
                );
            }
        }
    }
}

#[test]
fn reranker_remains_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<super::Reranker>();
}

/// `answers_key` per candidate, with no candidate an exact dictionary hit.
fn answering(flags: &[bool]) -> impl Fn(usize) -> CandidateFacts + '_ {
    move |index| CandidateFacts {
        answers_key: flags[index],
        trusted_dictionary_hit: false,
    }
}

#[test]
fn only_candidates_answering_the_key_are_compared() {
    // A prefix and a completion sit among the full answers and stay out of the comparison.
    let flags = [true, false, true, false, true];
    assert_eq!(
        comparable(5, &answering(&flags)).unwrap().as_slice(),
        &[0, 2, 4]
    );
}

#[test]
fn differing_character_counts_are_compared_when_both_answer_the_key() {
    // The case the old length proxy dropped: `xian` reads as 现 or as 西安, both consuming the
    // key. Nothing here looks at the text, which is the point — length is not the criterion.
    assert_eq!(
        comparable(2, &answering(&[true, true])).unwrap().as_slice(),
        &[0, 1]
    );
}

#[test]
fn comparable_indices_use_fixed_storage() {
    let result = comparable(3, &answering(&[true, false, true])).unwrap();
    assert_eq!(
        std::mem::size_of_val(&result),
        std::mem::size_of::<Compared>()
    );
}

#[test]
fn best_where_keeps_original_candidate_indices() {
    let model = cache_test_model();
    let texts = ["吗我", "你吗", "你好", "你很"];
    let considered = [texts[0], texts[2], texts[3]];
    let expected_scores = super::SentenceModel::score(&model, "我", &considered);
    let expected = expected_scores
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))
        .map(|(position, _)| [0, 2, 3][position])
        .expect("at least one comparable candidate");

    let mut reranker = super::Reranker::new(model);
    let actual = reranker.best_where("我", &texts, answering(&[true, false, true, true]));
    assert_eq!(actual, (expected != 0).then_some(expected));
}

#[test]
fn a_positive_margin_can_decline_a_promotion() {
    let texts = ["吗我", "你吗", "你好", "你很"];
    let facts = answering(&[true, false, true, true]);
    // An unreachable margin leaves the engine order alone.
    let mut reranker = super::Reranker::new(cache_test_model());
    assert_eq!(
        reranker.best_where_margin("我", &texts, &facts, f32::INFINITY),
        None
    );
    // A zero margin is exactly the old behavior.
    let mut reference = super::Reranker::new(cache_test_model());
    let expected = reference.best_where("我", &texts, &facts);
    let mut reranker = super::Reranker::new(cache_test_model());
    assert_eq!(
        reranker.best_where_margin("我", &texts, &facts, 0.0),
        expected
    );
}

#[test]
fn a_trusted_dictionary_hit_in_the_lead_declines_the_decision() {
    let facts = |index: usize| CandidateFacts {
        answers_key: true,
        trusted_dictionary_hit: index == 0,
    };
    assert_eq!(comparable(3, &facts), None);
    // The same hit anywhere but the lead is only another candidate; it does not veto.
    let trailing = |index: usize| CandidateFacts {
        answers_key: true,
        trusted_dictionary_hit: index == 2,
    };
    assert_eq!(comparable(3, &trailing).unwrap().as_slice(), &[0, 1, 2]);
}

#[test]
fn a_leader_that_does_not_answer_the_key_declines_the_decision() {
    // Promoting over a prefix would compare readings of two different keys.
    assert_eq!(comparable(3, &answering(&[false, true, true])), None);
}

#[test]
fn fewer_than_two_comparable_candidates_declines_the_decision() {
    assert_eq!(comparable(0, &answering(&[])), None);
    assert_eq!(comparable(1, &answering(&[true])), None);
    assert_eq!(comparable(3, &answering(&[true, false, false])), None);
}

#[test]
fn the_comparison_is_capped_and_keeps_the_leader() {
    let flags = [true; 12];
    let considered = comparable(12, &answering(&flags)).expect("all answer the key");
    assert_eq!(considered.as_slice().len(), super::COMPARED);
    assert_eq!(considered.as_slice()[0], 0);
}

#[test]
fn gate_follows_the_leading_candidate_only() {
    // Database and UserDatabase lead: the dictionary knows the whole key, leave it alone.
    assert!(!should_rerank(&[0, 8, 9]));
    assert!(!should_rerank(&[1, 8]));
    // Lattice or fallback leads: no frequency evidence, so the model is worth consulting.
    assert!(should_rerank(&[8, 0, 0]));
    assert!(should_rerank(&[9, 0]));
    // Nothing to reorder.
    assert!(!should_rerank(&[]));
}

#[test]
fn erf_matches_known_values() {
    assert!((erf(0.0) - 0.0).abs() < 1e-6);
    assert!((erf(0.5) - 0.520_499_9).abs() < 1e-6);
    assert!((erf(1.0) - 0.842_700_8).abs() < 1e-6);
    assert!((erf(-1.0) + 0.842_700_8).abs() < 1e-6);
    assert!((erf(3.0) - 0.999_977_9).abs() < 1e-6);
}

#[test]
fn gelu_matches_torch_reference() {
    // Values taken from torch.nn.functional.gelu with its default exact formulation.
    assert!((gelu(0.0) - 0.0).abs() < 1e-6);
    assert!((gelu(1.0) - 0.841_344_8).abs() < 1e-6);
    assert!((gelu(-1.0) + 0.158_655_25).abs() < 1e-6);
    assert!((gelu(2.0) - 1.954_499_7).abs() < 1e-6);
}
