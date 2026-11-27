//! Fixed-size candidate window and experimental post-commit suggestion bar evaluation.
//! Run `cargo run --release -p lueying-bench --example ui-order-eval -- --help`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::Path;
use std::sync::{Arc, RwLock};

use lueying_core::{Engine, Event, Fuzzy, Key, Script, UserDict, session, set_sim_now};
use lueying_dict::{Dictionary, gram::Gram};
use lueying_render::panel::page_bounds;
use lueying_server::config::AppConfig;
use serde_json::json;

#[allow(dead_code)]
#[path = "../src/bench.rs"]
mod bench;

const USAGE: &str = "ui-order-eval --dict PATH [--gram PATH] [--corpus PATH] \
    [--page-size 1..9] [--train PATH] [--suggestion-size 1..9] [--rows NEW_PATH]\n\
    Defaults: dicts/eval-shared-v1.tsv, configured product page size, 5 suggestions.\n\
    TSV: input/expected[/context] or category/source/input/expected[/context].\n\
    Prints JSON summary; --rows writes per-case metrics without input or expected text.\n\
    --train enables the hypothetical, unpaged NextWord bar; one key accepts any visible slot.";
const DEFAULT_CORPUS: &str = "dicts/eval-shared-v1.tsv";
const MAX_PICKS: usize = 16;
const MAX_WORD: usize = 4;

fn builtins() -> Arc<lueying_core::module::Modules> {
    Arc::new(lueying_core::features::builtins(Script::Simplified))
}

struct Case<'a> {
    line: usize,
    input: &'a str,
    expected: &'a str,
    context: &'a str,
}

fn read_cases(body: &str) -> Result<Vec<Case<'_>>, String> {
    let mut cases = Vec::new();
    for (i, row) in body.lines().enumerate() {
        if row.trim().is_empty() || row.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = row.split('\t').collect();
        let (input, expected, context) = match fields.as_slice() {
            [input, expected] | [_, _, input, expected] => (*input, *expected, ""),
            [input, expected, context] | [_, _, input, expected, context] => {
                (*input, *expected, *context)
            }
            _ => return Err(format!("invalid TSV at line {}", i + 1)),
        };
        if input.is_empty() || expected.is_empty() {
            return Err(format!("empty input or expected at line {}", i + 1));
        }
        cases.push(Case {
            line: i + 1,
            input,
            expected,
            context,
        });
    }
    if cases.is_empty() {
        return Err("no cases".into());
    }
    Ok(cases)
}

type Readings<'a> = HashMap<&'a str, (&'a str, u64)>;

// Left-to-right longest dictionary match over the trailing Han run. A punctuation
// or unknown final token is not a committed previous word for this simulation.
fn previous<'a>(context: &'a str, keys: &Readings<'_>) -> Option<&'a str> {
    let han = |c| matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}');
    let tail = context.rsplit(|c| !han(c)).next()?;
    let mut ends: Vec<_> = tail.char_indices().map(|(i, _)| i).collect();
    ends.push(tail.len());
    let mut at = 0;
    let mut last = None;
    while at + 1 < ends.len() {
        let stop = (at + 1..=(at + MAX_WORD).min(ends.len() - 1))
            .rev()
            .find(|&stop| keys.contains_key(&tail[ends[at]..ends[stop]]));
        last = stop.map(|stop| &tail[ends[at]..ends[stop]]);
        at = stop.unwrap_or(at + 1);
    }
    last
}

fn train(
    dict: &Arc<Dictionary>,
    keys: &Readings<'_>,
    training: &[Case<'_>],
    test: &[Case<'_>],
) -> (Engine, usize) {
    let held_out: HashSet<_> = test.iter().map(|c| (c.context, c.expected)).collect();
    let mut seen = HashSet::new();
    let examples: Vec<_> = training
        .iter()
        .filter(|c| {
            c.expected.chars().count() <= MAX_WORD
                && !held_out.contains(&(c.context, c.expected))
                && seen.insert((c.context, c.expected))
        })
        .filter_map(|c| {
            let prev = previous(c.context, keys)?;
            let (key, _) = keys.get(c.expected)?;
            Some((prev, *key, c.expected.to_owned()))
        })
        .collect();
    let mut user = UserDict::new();
    let mut clock = 1_800_000_000;
    // Mirror the route-6 two-pass warm profile; keep it apart from candidate ranking.
    for _ in 0..2 {
        for (prev, key, next) in &examples {
            set_sim_now(clock);
            clock += 120;
            user.learn(key, next);
            user.learn_transitions(Some(prev), std::slice::from_ref(next));
        }
    }
    set_sim_now(clock);
    let mut engine =
        Engine::new(Arc::clone(dict), Default::default()).with_user(Arc::new(RwLock::new(user)));
    engine.set_learning(false);
    (engine, examples.len())
}

fn ranks(engine: &Engine, expected: &str) -> (Option<usize>, Option<usize>) {
    let legacy = bench::eval_texts(engine)
        .iter()
        .position(|text| bench::matches_normalized(text, expected));
    let mut view = Default::default();
    session::refresh(engine, &mut view);
    let visible = view
        .candidates
        .iter()
        .zip(engine.candidates_detailed())
        .position(|(text, cand)| {
            cand.action.is_none() && bench::matches_normalized(text, expected)
        });
    (legacy, visible)
}

fn location(index: usize, page_size: usize, len: usize) -> (usize, usize) {
    let page = index / page_size;
    (page, index - page_bounds(page, page_size, len).start)
}

/// Type first, then mimic the shared benchmark's longest-prefix choice.
/// Count a path only if the physical keys actually commit the expected text and finish.
fn finish(engine: &mut Engine, expected: &str) -> Option<(usize, usize)> {
    let mut remaining = expected;
    let mut flips = 0;
    for pick in 1..=MAX_PICKS {
        let candidates = engine.candidates_detailed();
        let (index, prefix) = candidates
            .iter()
            .enumerate()
            .filter(|(_, cand)| cand.action.is_none())
            .filter_map(|(i, cand)| {
                let text = bench::normalize(&cand.text);
                (!text.is_empty() && remaining.starts_with(&text)).then_some((i, text))
            })
            .max_by_key(|(i, text)| (text.chars().count(), std::cmp::Reverse(*i)))?;
        let (page, slot) = location(index, engine.page_size(), candidates.len());
        let start = engine.page();
        let navigation = if page < start {
            Key::PageUp
        } else {
            Key::PageDown
        };
        for _ in 0..start.abs_diff(page) {
            if !engine.process(navigation).is_empty() {
                return None;
            }
            flips += 1;
        }
        if engine.page() != page {
            return None;
        }
        let events = engine.process(Key::Digit((slot + 1) as u8));
        let [Event::Commit(text)] = events.as_slice() else {
            return None;
        };
        if !bench::matches_normalized(text, &prefix) {
            return None;
        }
        remaining = &remaining[prefix.len()..];
        if remaining.is_empty() {
            return engine.composition().is_none().then_some((pick, flips));
        }
    }
    None
}

#[derive(Default)]
struct RankCounts {
    top1: usize,
    top5: usize,
    found: usize,
}

impl RankCounts {
    fn add(&mut self, index: Option<usize>) {
        self.top1 += usize::from(index == Some(0));
        self.top5 += usize::from(index.is_some_and(|i| i < 5));
        self.found += usize::from(index.is_some());
    }

    fn json(self) -> serde_json::Value {
        json!({"top1": self.top1, "top5": self.top5, "found": self.found})
    }
}

fn format_opt(value: Option<usize>) -> String {
    value.map_or_else(|| "-".into(), |n| n.to_string())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut flags = HashMap::new();
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            println!("{USAGE}");
            return Ok(());
        }
        if !matches!(
            flag.as_str(),
            "--dict"
                | "--gram"
                | "--corpus"
                | "--page-size"
                | "--train"
                | "--suggestion-size"
                | "--rows"
        ) || flags.contains_key(&flag)
        {
            return Err(format!("unknown or repeated option: {flag}").into());
        }
        let value = args
            .next()
            .filter(|s| !s.starts_with("--"))
            .ok_or_else(|| format!("missing value: {flag}"))?;
        flags.insert(flag, value);
    }
    let size = |flag: &str, default| -> Result<usize, String> {
        flags.get(flag).map_or(Ok(default), |s| {
            s.parse::<usize>()
                .ok()
                .filter(|n| (1..=9).contains(n))
                .ok_or_else(|| format!("{flag} must be in 1..=9"))
        })
    };
    let page_size = size("--page-size", AppConfig::default().page_size)?;
    let suggestion_size = size("--suggestion-size", 5)?;
    if flags.contains_key("--suggestion-size") && !flags.contains_key("--train") {
        return Err("--suggestion-size requires --train".into());
    }
    let corpus = flags.get("--corpus").map_or(DEFAULT_CORPUS, String::as_str);
    let body = std::fs::read_to_string(corpus)?;
    let cases = read_cases(&body).map_err(|err| format!("{corpus}: {err}"))?;
    let dict_path = flags.get("--dict").ok_or("--dict is required")?;
    let (dict, _) = Dictionary::from_path(Path::new(dict_path))?;
    let dict = Arc::new(dict);
    let gram = flags
        .get("--gram")
        .map(|path| Gram::open(Path::new(path)).map(Arc::new))
        .transpose()?;
    let mut keys = Readings::new();
    let predictor = if let Some(path) = flags.get("--train") {
        for entry in dict
            .iter()
            .filter(|e| !e.word.is_empty() && !e.key.is_empty())
        {
            let reading = keys.entry(entry.word).or_insert((entry.key, entry.freq));
            if entry.freq > reading.1 {
                *reading = (entry.key, entry.freq);
            }
        }
        let training = std::fs::read_to_string(path)?;
        let rows = read_cases(&training).map_err(|err| format!("{path}: {err}"))?;
        let (predictor, trained) = train(&dict, &keys, &rows, &cases);
        if trained == 0 {
            return Err(
                "no usable training rows after filtering held-out context/answer pairs".into(),
            );
        }
        Some((predictor, trained))
    } else {
        None
    };
    let mut output = flags
        .get("--rows")
        .map(|path| File::create_new(path).map(BufWriter::new))
        .transpose()?;
    if let Some(out) = &mut output {
        writeln!(
            out,
            "line\tengine_rank\tui_rank\tui_page\tui_slot\tui_flips\tinput_keys\tpicks\tpath_flips\tpath_keys\tbar_eligible\tbar_shown\tbar_slot\twith_bar_keys"
        )?;
    }

    let mut engine = bench::make_engine(&dict, &gram, Fuzzy::auto());
    engine.set_page_size(page_size);
    engine.set_learning(false);
    let (mut legacy, mut ui) = (RankCounts::default(), RankCounts::default());
    let (mut page_counts, mut slot_counts) = (BTreeMap::new(), BTreeMap::new());
    let (mut paths, mut key_sum, mut flip_sum, mut pick_sum) = (0, 0, 0, 0);
    let (mut eligible, mut shown, mut hits, mut first_hits) = (0, 0, 0, 0);
    let (mut saved, mut rescued) = (0, 0);
    for case in &cases {
        engine.process(Key::Esc);
        engine.clear_context();
        engine.set_context(case.context);
        let mut input_keys = 0;
        let mut typed_without_commit = true;
        for c in case.input.chars() {
            typed_without_commit &= engine.process(Key::Letter(c)).is_empty();
            input_keys += 1;
        }
        let expected = bench::normalize(case.expected);
        let (engine_index, ui_index) = ranks(&engine, &expected);
        legacy.add(engine_index);
        ui.add(ui_index);
        let ui_loc = ui_index.map(|i| location(i, page_size, engine.candidates_detailed().len()));
        if let Some((page, slot)) = ui_loc {
            *page_counts.entry(page + 1).or_insert(0usize) += 1;
            *slot_counts.entry(slot + 1).or_insert(0usize) += 1;
        }
        let path = typed_without_commit
            .then(|| finish(&mut engine, &expected))
            .flatten();
        let path_keys = path.map(|(picks, flips)| input_keys + picks + flips);
        if let Some(((picks, flips), total)) = path.zip(path_keys) {
            paths += 1;
            key_sum += total;
            flip_sum += flips;
            pick_sum += picks;
        }

        let mut bar_shown = false;
        let prev = predictor
            .as_ref()
            .and_then(|_| previous(case.context, &keys));
        let bar_slot = predictor
            .as_ref()
            .zip(prev)
            .and_then(|((engine, _), prev)| {
                let suggestions = engine.predict_after(prev, suggestion_size);
                bar_shown = !suggestions.is_empty();
                suggestions
                    .iter()
                    .position(|word| word.key.is_some() && word.text == case.expected)
            });
        eligible += usize::from(prev.is_some());
        shown += usize::from(bar_shown);
        hits += usize::from(bar_slot.is_some());
        first_hits += usize::from(bar_slot == Some(0));
        let with_bar_keys = if bar_slot.is_some() {
            if let Some(total) = path_keys {
                saved += total - 1;
            } else {
                rescued += 1;
            }
            Some(1)
        } else {
            path_keys
        };
        if let Some(out) = &mut output {
            writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{input_keys}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                case.line,
                format_opt(engine_index.map(|n| n + 1)),
                format_opt(ui_index.map(|n| n + 1)),
                format_opt(ui_loc.map(|(page, _)| page + 1)),
                format_opt(ui_loc.map(|(_, slot)| slot + 1)),
                format_opt(ui_loc.map(|(page, _)| page)),
                format_opt(path.map(|(picks, _)| picks)),
                format_opt(path.map(|(_, flips)| flips)),
                format_opt(path_keys),
                usize::from(prev.is_some()),
                usize::from(bar_shown),
                format_opt(bar_slot.map(|n| n + 1)),
                format_opt(with_bar_keys),
            )?;
        }
    }
    if let Some(mut out) = output {
        out.flush()?;
    }
    let summary = json!({
        "cases": cases.len(), "page_size": page_size,
        "engine_order": legacy.json(),
        "ui_order": {
            "ranks": ui.json(), "first_page_hits": page_counts.get(&1).copied().unwrap_or(0),
            "page_counts": page_counts, "slot_counts": slot_counts,
        },
        "typing": {
            "successful_cases": paths, "key_total": key_sum, "selection_keys": pick_sum,
            "page_keys": flip_sum, "key_total_without_paging": key_sum - flip_sum,
        },
        "next_word": predictor.as_ref().map(|(_, trained)| json!({
            "training_examples": trained, "size": suggestion_size,
            "eligible": eligible, "shown": shown, "top1_hits": first_hits, "hits": hits,
            "rescued_typing_misses": rescued,
            "saved_keys_on_typing_successes": saved,
            "key_total_on_same_typing_successes": key_sum - saved,
        })),
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}\n{USAGE}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use lueying_core::Action;

    use super::*;

    #[test]
    fn visible_action_moves_a_text_hit_but_cannot_itself_be_a_text_hit() {
        let dict = Arc::new(Dictionary::builtin());
        let mut engine = bench::make_engine(&dict, &None, Fuzzy::auto());
        engine.set_learning(false);
        engine.set_page_size(1);
        engine.set_action(Some(Action {
            trigger: "yi".into(),
            id: "demo.open".into(),
            title: "Open Settings".into(),
        }));
        for key in "yi".chars() {
            engine.process(Key::Letter(key));
        }
        let (legacy, visible) = ranks(&engine, "\u{4ee5}");
        let index = visible.expect("the text candidate is visible");
        assert!(index > legacy.expect("legacy text candidate"));
        assert_eq!(ranks(&engine, "Open Settings"), (None, None));
        assert_eq!(finish(&mut engine, "\u{4ee5}"), Some((1, index)));
    }
}
