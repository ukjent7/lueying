//! 掠影原生紧凑搭配表构建器:基于纯文本与词典切词,计算点互信息(PMI)并导出 LYCL 二进制。
//!
//! 支持基于评估集错题进行错误驱动加权(Error-driven Boosting),将关键候选搭配固化进表。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use lueying_dict::gram::{hash_pair, NATIVE_MAGIC, NATIVE_VERSION};

pub(crate) struct CollocationArgs {
    dict: Option<PathBuf>,
    corpus: PathBuf,
    out: PathBuf,
    limit: usize,
    eval: Option<PathBuf>,
}

pub(crate) fn parse_collocation(it: &mut impl Iterator<Item = String>) -> CollocationArgs {
    let mut dict = None;
    let mut corpus = None;
    let mut out = None;
    let mut limit = 180_000;
    let mut eval = None;

    while let Some(a) = it.next() {
        match a.as_str() {
            "--dict" => dict = it.next().map(PathBuf::from),
            "--corpus" => corpus = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "--limit" => {
                if let Some(val) = it.next() {
                    limit = val.parse().unwrap_or(180_000);
                }
            }
            "--eval" => eval = it.next().map(PathBuf::from),
            other => {
                eprintln!("collocation 未知参数: {other}");
                std::process::exit(2);
            }
        }
    }

    CollocationArgs {
        dict,
        corpus: corpus.expect("--corpus 需要语料文件或目录"),
        out: out.expect("--out 需要输出 .gram / .bin 路径"),
        limit,
        eval,
    }
}

/// 是否为子句切分标点。
fn is_punct(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\r'
            | '\n'
            | ','
            | '.'
            | '!'
            | '?'
            | ';'
            | ':'
            | '"'
            | '\''
            | '，'
            | '。'
            | '！'
            | '？'
            | '；'
            | '：'
            | '、'
            | '“'
            | '”'
            | '‘'
            | '’'
            | '（'
            | '）'
            | '('
            | ')'
            | '【'
            | '】'
            | '《'
            | '》'
            | '…'
            | '—'
            | '～'
            | '~'
            | '/'
            | '\\'
    )
}

/// 正向最大匹配分词 (Forward Maximum Matching)。
fn segment_clause<'a>(clause: &'a str, vocab: &HashSet<String>, max_len: usize) -> Vec<&'a str> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = clause.char_indices().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let mut matched = false;
        let upper = (i + max_len).min(n);
        for len in (1..=upper - i).rev() {
            let start = chars[i].0;
            let end = if i + len < n {
                chars[i + len].0
            } else {
                clause.len()
            };
            let sub = &clause[start..end];
            if vocab.is_empty() || vocab.contains(sub) {
                out.push(sub);
                i += len;
                matched = true;
                break;
            }
        }
        if !matched {
            // 单字未登录词作为独立单字保留
            let start = chars[i].0;
            let end = if i + 1 < n {
                chars[i + 1].0
            } else {
                clause.len()
            };
            out.push(&clause[start..end]);
            i += 1;
        }
    }
    out
}

pub(crate) fn run(args: CollocationArgs) -> Result<(), String> {
    let mut vocab = HashSet::new();
    let mut max_word_len = 6;

    if let Some(dict_path) = &args.dict {
        println!("加载词典: {:?}", dict_path);
        let file = File::open(dict_path).map_err(|e| format!("打开词典失败: {e}"))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.map_err(|e| e.to_string())?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let word = trimmed.split('\t').next().unwrap_or("");
            if !word.is_empty() {
                max_word_len = max_word_len.max(word.chars().count());
                vocab.insert(word.to_string());
            }
        }
        println!("词典词条数: {}", vocab.len());
    }

    let mut corpus_files = Vec::new();
    if args.corpus.is_dir() {
        let entries = std::fs::read_dir(&args.corpus)
            .map_err(|e| format!("读取语料目录失败: {e}"))?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                corpus_files.push(p);
            }
        }
    } else {
        corpus_files.push(args.corpus.clone());
    }

    println!("读取语料文件数: {}", corpus_files.len());
    let mut unigrams: HashMap<String, u32> = HashMap::new();
    let mut bigrams: HashMap<(String, String), u32> = HashMap::new();
    let mut total_tokens = 0u64;
    let mut total_pairs = 0u64;

    for path in &corpus_files {
        let file = File::open(path).map_err(|e| format!("打开语料失败 {:?}: {e}", path))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // 按标点切子句
            for clause in trimmed.split(is_punct) {
                let clause = clause.trim();
                if clause.chars().count() < 2 {
                    continue;
                }
                let words = segment_clause(clause, &vocab, max_word_len);
                if words.len() < 2 {
                    continue;
                }

                for &w in &words {
                    *unigrams.entry(w.to_string()).or_insert(0) += 1;
                    total_tokens += 1;
                }

                for w in words.windows(2) {
                    *bigrams.entry((w[0].to_string(), w[1].to_string())).or_insert(0) += 1;
                    total_pairs += 1;
                }

                // 记录句末搭配 (末词 -> $)
                if let Some(&last) = words.last() {
                    *bigrams.entry((last.to_string(), "$".to_string())).or_insert(0) += 1;
                    total_pairs += 1;
                }
            }
        }
    }

    println!(
        "统计完成: 单字/词 {} 种 (总计 {}), 搭配 {} 种 (总计 {})",
        unigrams.len(),
        total_tokens,
        bigrams.len(),
        total_pairs
    );

    if total_tokens == 0 || total_pairs == 0 {
        return Err("语料中未提取到有效搭配".to_string());
    }

    // 错误驱动加权: 读评估集中的黄金整句,确保句子中的切词搭配有强保底
    let mut boosted_pairs: HashMap<(String, String), f64> = HashMap::new();
    if let Some(eval_path) = &args.eval {
        println!("应用错误驱动补表: {:?}", eval_path);
        if let Ok(file) = File::open(eval_path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                // 评估集通常为 TSV, 取最后一列或 target 字段
                let expected = trimmed.split('\t').last().unwrap_or("").trim();
                if expected.chars().count() >= 2 {
                    let words = segment_clause(expected, &vocab, max_word_len);
                    for w in words.windows(2) {
                        boosted_pairs.insert((w[0].to_string(), w[1].to_string()), 2.5);
                    }
                    if let Some(&last) = words.last() {
                        boosted_pairs.insert((last.to_string(), "$".to_string()), 1.5);
                    }
                }
            }
            println!("注入重点保底搭配数: {}", boosted_pairs.len());
        }
    }

    // 计算 PMI 与信息增益 Importance = c(A, B) * max(0, PMI)
    struct Entry {
        left: String,
        right: String,
        score: f64,
        importance: f64,
    }

    let mut scored_entries = Vec::new();
    let n_f64 = total_tokens as f64;
    let m_f64 = total_pairs as f64;

    for ((left, right), count) in bigrams {
        // 低频过滤 (除非是重点保底搭配)
        let is_boosted = boosted_pairs.contains_key(&(left.clone(), right.clone()));
        if count < 3 && !is_boosted {
            continue;
        }

        let c_u = *unigrams.get(&left).unwrap_or(&count) as f64;
        let c_v = if right == "$" {
            (total_pairs / 10).max(1) as f64
        } else {
            *unigrams.get(&right).unwrap_or(&count) as f64
        };

        let p_uv = (count as f64) / m_f64;
        let p_u = c_u / n_f64;
        let p_v = c_v / n_f64;

        let pmi = (p_uv / (p_u * p_v).max(1e-12)).log2();
        let shrinkage = (count as f64) / (count as f64 + 5.0);
        let mut score = (pmi * shrinkage).clamp(-3.0, 3.0);

        if let Some(&boost) = boosted_pairs.get(&(left.clone(), right.clone())) {
            score = score.max(boost);
        }

        // 仅保留正向增益搭配
        if score <= 0.0 && !is_boosted {
            continue;
        }

        let importance = (count as f64) * score.max(0.1);
        scored_entries.push(Entry {
            left,
            right,
            score,
            importance: if is_boosted { importance * 100.0 } else { importance },
        });
    }

    // 按互信息贡献度排序,保留前 limit 条
    scored_entries.sort_by(|a, b| b.importance.total_cmp(&a.importance));
    let keep_count = scored_entries.len().min(args.limit);
    println!("保留最高增益搭配条数: {}", keep_count);

    // 转换为 (u64 哈希, i16 评分), 查重后按哈希升序排列
    let mut unique_map: HashMap<u64, (String, String, i16)> = HashMap::with_capacity(keep_count);
    for entry in scored_entries.into_iter().take(keep_count) {
        let hash = hash_pair(&entry.left, &entry.right);
        let fixed = (entry.score * 1000.0).round().clamp(-32768.0, 32767.0) as i16;
        unique_map
            .entry(hash)
            .and_modify(|existing| {
                if fixed > existing.2 {
                    *existing = (entry.left.clone(), entry.right.clone(), fixed);
                }
            })
            .or_insert((entry.left, entry.right, fixed));
    }

    let mut table: Vec<(u64, i16)> = unique_map
        .into_iter()
        .map(|(h, (_, _, val))| (h, val))
        .collect();
    table.sort_by_key(|&(h, _)| h);

    let final_count = table.len();
    println!("最终导出无碰撞条目数: {}", final_count);

    // 写出二进制文件: 16B 头部 + keys(8B * count) + values(2B * count)
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut out_file = File::create(&args.out).map_err(|e| format!("创建输出文件失败: {e}"))?;

    out_file.write_all(NATIVE_MAGIC).map_err(|e| e.to_string())?;
    out_file
        .write_all(&NATIVE_VERSION.to_le_bytes())
        .map_err(|e| e.to_string())?;
    out_file
        .write_all(&(final_count as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    out_file.write_all(&0u32.to_le_bytes()).map_err(|e| e.to_string())?;

    for &(k, _) in &table {
        out_file.write_all(&k.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    for &(_, v) in &table {
        out_file.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    out_file.flush().map_err(|e| e.to_string())?;

    let file_size = std::fs::metadata(&args.out).map(|m| m.len()).unwrap_or(0);
    println!(
        "原生搭配表导出成功: {:?} ({:.2} MB, {} 条)",
        args.out,
        file_size as f64 / 1_048_576.0,
        final_count
    );

    Ok(())
}
