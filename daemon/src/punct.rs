//! Punctuation for Japanese transcripts.
//!
//! The CTC model rarely punctuates a long decode, so marks are added here.
//! They go at clause boundaries found by morphological analysis, not at
//! pauses: speakers pause to think in the middle of a sentence, pause length
//! does not predict commas (Akita & Kawahara 2011), and pauses correlate
//! poorly with clause boundaries (Shitaoka et al. 2004). A pause only
//! confirms a weak comma site such as a te-form.

use std::borrow::Cow;
use std::sync::LazyLock;

use anyhow::{anyhow, Result};
use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;

use crate::asr::sanitize;
use crate::sherpa::Transcript;

/// Gap between the starts of two tokens that counts as a pause. On TTS
/// speech, tokens inside a sentence started at most 0.56 s apart (a spoken
/// comma included) and tokens across a 0.7 s pause 1.5 s apart.
const PAUSE_SECONDS: f32 = 0.9;
const MARKS: &[char] = &['、', '。', '，', '．', ',', '.', '!', '?', '！', '？'];

/// Nouns that open a sentence after a plain form, which otherwise modifies
/// the noun: 思う。明日 but いい天気.
const OPENING_NOUNS: &[&str] = &[
    "今日",
    "きょう",
    "明日",
    "あした",
    "あす",
    "昨日",
    "きのう",
    "今",
    "いま",
    "今朝",
    "今夜",
    "今晩",
    "今週",
    "来週",
    "先週",
    "今月",
    "来月",
    "先月",
    "今年",
    "来年",
    "去年",
    "午前",
    "午後",
    "朝",
    "昼",
    "夜",
    "毎日",
    "最近",
    "さっき",
    "先ほど",
    "後で",
    "あとで",
    "今度",
    "次",
    "週末",
    "夕方",
];

static SEGMENTER: LazyLock<std::result::Result<Segmenter, String>> = LazyLock::new(|| {
    let dictionary = load_dictionary("embedded://ipadic").map_err(|e| format!("ipadic: {e}"))?;
    Ok(Segmenter::new(Mode::Normal, dictionary, None))
});

/// The IPADIC segmenter, loaded on first use and shared with the corrector.
pub fn segmenter() -> Result<&'static Segmenter> {
    SEGMENTER.as_ref().map_err(|e| anyhow!("{e}"))
}

/// One IPADIC word of the transcript.
struct Word {
    /// Byte offset past the word in the text.
    end: usize,
    surface: String,
    pos: String,
    sub: String,
    sub2: String,
    form: String,
    base: String,
    closing: Closing,
}

/// What may follow a word.
enum Boundary {
    Stop,
    Comma,
    CommaIfPaused,
    None,
}

/// Whether a sentence can end on a word.
#[derive(Clone, Copy, PartialEq)]
enum Closing {
    No,
    /// A plain form, which may also modify a following noun.
    Plain,
    /// A form that cannot modify a noun.
    Final,
}

/// `transcript.text` with marks at its clause boundaries.
pub fn punctuate(transcript: &Transcript) -> Result<String> {
    let (text, pauses) = text_and_pauses(transcript);
    let words = analyse(segmenter()?, &text)?;
    let mut out = String::with_capacity(text.len() + 30);
    let mut from = 0;
    for (i, word) in words.iter().enumerate() {
        out.push_str(&text[from..word.end]);
        from = word.end;
        let mark = match boundary(word, words.get(i + 1)) {
            Boundary::Stop => full_stop(&words[..=i]),
            Boundary::Comma => '、',
            Boundary::CommaIfPaused if pauses.contains(&word.end) => '、',
            Boundary::CommaIfPaused | Boundary::None => continue,
        };
        if !touches_mark(&text, word.end) {
            out.push(mark);
        }
    }
    out.push_str(&text[from..]);
    Ok(out)
}

fn boundary(word: &Word, next: Option<&Word>) -> Boundary {
    let Some(next) = next else {
        return if dangles(word) {
            Boundary::None
        } else {
            Boundary::Stop
        };
    };
    if continues(next) {
        return Boundary::None;
    }
    let closing = word.closing;
    if closing != Closing::No {
        return match next.pos.as_str() {
            "接続詞" | "副詞" | "感動詞" | "フィラー" | "連体詞" | "動詞" | "形容詞" => {
                Boundary::Stop
            }
            // A prefix starts a noun: 行くお店 is a relative clause.
            "名詞" | "接頭詞"
                if closing == Closing::Final
                    || next.sub == "代名詞"
                    || OPENING_NOUNS.contains(&next.surface.as_str()) =>
            {
                Boundary::Stop
            }
            _ => Boundary::None,
        };
    }
    match (word.pos.as_str(), word.sub.as_str(), word.surface.as_str()) {
        ("接続詞" | "感動詞" | "フィラー", _, _) => Boundary::Comma,
        (
            "助詞",
            "接続助詞",
            "けど" | "けれど" | "けれども" | "が" | "から" | "ので" | "のに" | "し",
        ) => Boundary::Comma,
        ("助詞", "接続助詞", "て" | "で" | "ば") => Boundary::CommaIfPaused,
        ("助動詞", _, _) if matches!(word.base.as_str(), "た" | "だ") && word.form == "仮定形" => {
            Boundary::CommaIfPaused
        }
        // 連用中止. Before a verb, a 連用形 more likely forms a compound verb.
        ("動詞", "自立", _) if word.form == "連用形" && next.pos != "動詞" => {
            Boundary::CommaIfPaused
        }
        _ => Boundary::None,
    }
}

fn closing(prev: Option<&Word>, word: &Word) -> Closing {
    // IPADIC tags ください before a noun as 連用形.
    if word.form.starts_with("命令") || matches!(word.surface.as_str(), "ください" | "下さい")
    {
        return Closing::Final;
    }
    match word.pos.as_str() {
        "助詞" if word.sub.contains("終助詞") => Closing::Final,
        "助動詞" if matches!(word.base.as_str(), "う" | "よう" | "まい") => Closing::Final,
        "助動詞"
            if word.form == "基本形" && matches!(word.base.as_str(), "です" | "ます" | "だ") =>
        {
            Closing::Final
        }
        // Unlike した and しない, でした and ません can't modify a noun.
        "助動詞"
            if word.form == "基本形"
                && matches!(word.base.as_str(), "た" | "ん")
                && prev.is_some_and(|p| {
                    p.pos == "助動詞" && matches!(p.base.as_str(), "ます" | "です")
                }) =>
        {
            Closing::Final
        }
        // Before a noun, みたい would need な.
        "名詞" if word.sub == "非自立" && word.sub2 == "形容動詞語幹" => Closing::Final,
        "動詞" | "形容詞" | "助動詞" if word.form == "基本形" => Closing::Plain,
        _ => Closing::No,
    }
}

/// Whether `word` attaches to the word before it: ですね, 行くこと, してください.
fn continues(word: &Word) -> bool {
    matches!(word.pos.as_str(), "助詞" | "助動詞")
        || matches!(word.sub.as_str(), "非自立" | "接尾" | "特殊")
}

/// Whether the text can't end on `word`: a particle that needs more after it
/// (を, ので), but not a te-form, because a dictated request often ends on it.
fn dangles(word: &Word) -> bool {
    match word.pos.as_str() {
        "助詞" => {
            !word.sub.contains("終助詞")
                && !(word.sub == "接続助詞" && matches!(word.surface.as_str(), "て" | "で"))
        }
        "接頭詞" => true,
        _ => false,
    }
}

/// ？ when the particles and auxiliaries ending `sentence` include か.
fn full_stop(sentence: &[Word]) -> char {
    let question = sentence
        .iter()
        .rev()
        .take_while(|w| matches!(w.pos.as_str(), "助詞" | "助動詞"))
        .any(|w| w.surface == "か" && w.sub.contains("終助詞"));
    if question {
        '？'
    } else {
        '。'
    }
}

/// The transcript text and the byte offsets in it where the speaker paused.
/// Without tokens that spell the text, there are no pauses.
fn text_and_pauses(transcript: &Transcript) -> (String, Vec<usize>) {
    let tokens = &transcript.tokens;
    let mut text = String::new();
    let mut pauses = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        text.push_str(&token.text.replace('▁', " "));
        if tokens
            .get(i + 1)
            .is_some_and(|next| next.start - token.start >= PAUSE_SECONDS)
        {
            pauses.push(text.trim_end().len());
        }
    }
    let joined = sanitize(&text);
    if joined != transcript.text {
        return (transcript.text.clone(), Vec::new());
    }
    let lead = text.len() - text.trim_start().len();
    (
        joined,
        pauses.into_iter().map(|p| p.saturating_sub(lead)).collect(),
    )
}

fn touches_mark(text: &str, at: usize) -> bool {
    text[..at].trim_end().ends_with(MARKS) || text[at..].trim_start().starts_with(MARKS)
}

fn analyse(segmenter: &Segmenter, text: &str) -> Result<Vec<Word>> {
    let mut words: Vec<Word> = segmenter
        .segment(Cow::Borrowed(text))
        .map_err(|e| anyhow!("morphological analysis: {e}"))?
        .into_iter()
        .map(|mut t| Word {
            end: t.byte_end,
            pos: t.get_detail(0).unwrap_or_default().to_string(),
            sub: t.get_detail(1).unwrap_or_default().to_string(),
            sub2: t.get_detail(2).unwrap_or_default().to_string(),
            form: t.get_detail(5).unwrap_or_default().to_string(),
            base: t.get_detail(6).unwrap_or_default().to_string(),
            surface: t.surface.into_owned(),
            closing: Closing::No,
        })
        .collect();
    // IPADIC tags でも, だ and か by surface; the neighbours tell the
    // conjunctions (高い。でも, です。だから) and the question (行くのか) from
    // the particles and the copula (雨でも, 雨だから, 何か).
    for i in 0..words.len() {
        let (before, rest) = words.split_at_mut(i);
        let (word, after) = rest.split_first_mut().expect("i < words.len()");
        let prev = before.last();
        let predicate =
            prev.is_some_and(|p| matches!(p.pos.as_str(), "動詞" | "形容詞" | "助動詞"));
        let conjunction = predicate
            && prev.is_some_and(|p| p.closing != Closing::No)
            && matches!(
                (word.surface.as_str(), word.pos.as_str()),
                ("でも", "助詞") | ("だ", "助動詞")
            );
        if conjunction {
            word.pos = "接続詞".into();
        } else if word.surface == "か"
            && word.sub.contains("終助詞")
            && (!(predicate || prev.is_some_and(continues))
                || after.first().is_some_and(|n| n.surface == "どう"))
        {
            word.sub = "副助詞".into();
        }
        word.closing = closing(prev, word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sherpa::Token;

    /// `parts` are token texts, each with the seconds of silence before the next.
    fn transcript(parts: &[(&str, f32)]) -> Transcript {
        let mut start = 0.0;
        let mut tokens = Vec::new();
        for &(text, pause) in parts {
            tokens.push(Token {
                text: text.into(),
                start,
            });
            start += 0.2 + pause;
        }
        let text = sanitize(
            &tokens
                .iter()
                .map(|t| t.text.replace('▁', " "))
                .collect::<String>(),
        );
        Transcript {
            text,
            confidence: None,
            tokens,
        }
    }

    #[test]
    fn thinking_pauses_inside_a_phrase_get_no_mark() {
        let t = transcript(&[("▁きょうは", 1.0), ("いい", 1.0), ("天気です", 0.0)]);
        assert_eq!(punctuate(&t).unwrap(), "きょうはいい天気です。");
    }

    #[test]
    fn a_sentence_end_gets_a_full_stop_without_a_pause() {
        let t = transcript(&[("▁確認してください昨日の会議はどうでしたか", 0.0)]);
        assert_eq!(
            punctuate(&t).unwrap(),
            "確認してください。昨日の会議はどうでしたか？"
        );
    }

    #[test]
    fn a_relative_clause_gets_no_full_stop_even_after_a_pause() {
        let t = transcript(&[("▁昨日買った", 1.0), ("本は高い", 0.0)]);
        assert_eq!(punctuate(&t).unwrap(), "昨日買った本は高い。");
    }

    #[test]
    fn a_plain_form_before_a_time_word_ends_the_sentence() {
        for (text, marked) in [
            (
                "タイムアウトだと思う明日直します",
                "タイムアウトだと思う。明日直します。",
            ),
            ("行くと思う週末に話します", "行くと思う。週末に話します。"),
            (
                "晴れると思う夕方には戻ります",
                "晴れると思う。夕方には戻ります。",
            ),
        ] {
            let t = transcript(&[(text, 0.0)]);
            assert_eq!(punctuate(&t).unwrap(), marked);
        }
    }

    #[test]
    fn a_polite_past_or_negative_ends_the_sentence_before_a_noun() {
        for (text, marked) in [
            (
                "予定が変更になりました会議室は三階です",
                "予定が変更になりました。会議室は三階です。",
            ),
            (
                "荷物が届いていません配送状況を確認します",
                "荷物が届いていません。配送状況を確認します。",
            ),
        ] {
            let t = transcript(&[(text, 0.0)]);
            assert_eq!(punctuate(&t).unwrap(), marked);
        }
    }

    #[test]
    fn mitai_ends_the_sentence_before_a_new_clause() {
        let t = transcript(&[("まだ決まっていないみたい決まったら教えて", 0.0)]);
        assert_eq!(
            punctuate(&t).unwrap(),
            "まだ決まっていないみたい。決まったら教えて。"
        );
    }

    #[test]
    fn a_conjunctive_particle_gets_a_comma_without_a_pause() {
        let t = transcript(&[("▁雨だけど行きますので待ってください", 0.0)]);
        assert_eq!(
            punctuate(&t).unwrap(),
            "雨だけど、行きますので、待ってください。"
        );
    }

    #[test]
    fn a_te_form_gets_a_comma_only_after_a_pause() {
        let paused = transcript(&[("▁テストを実行して", 1.0), ("結果を確認して", 0.0)]);
        assert_eq!(
            punctuate(&paused).unwrap(),
            "テストを実行して、結果を確認して。"
        );
        let fluent = transcript(&[("▁テストを実行して", 0.3), ("結果を確認して", 0.0)]);
        assert_eq!(
            punctuate(&fluent).unwrap(),
            "テストを実行して結果を確認して。"
        );
    }

    #[test]
    fn a_conjunction_ipadic_splits_after_a_predicate_starts_a_sentence() {
        for (text, marked) in [
            ("高いでもおもしろい", "高い。でも、おもしろい。"),
            ("中止ですだから家にいます", "中止です。だから、家にいます。"),
        ] {
            let t = transcript(&[(text, 0.0)]);
            assert_eq!(punctuate(&t).unwrap(), marked);
        }
    }

    #[test]
    fn ka_is_a_question_only_after_a_predicate() {
        for (text, marked) in [
            ("行くのか", "行くのか？"),
            ("何か食べる", "何か食べる。"),
            ("行くかどうか決める", "行くかどうか決める。"),
        ] {
            let t = transcript(&[(text, 0.0)]);
            assert_eq!(punctuate(&t).unwrap(), marked);
        }
    }

    #[test]
    fn the_end_gets_no_full_stop_after_a_particle_that_continues() {
        for text in ["資料を", "雨なので"] {
            let t = transcript(&[(text, 0.0)]);
            assert_eq!(punctuate(&t).unwrap(), text);
        }
    }

    #[test]
    fn marks_the_model_emitted_are_not_doubled() {
        let t = transcript(&[("▁始めます。", 1.0), ("いいですか?", 0.0)]);
        assert_eq!(punctuate(&t).unwrap(), "始めます。いいですか?");
    }

    #[test]
    fn a_pause_inside_a_word_gets_no_mark() {
        let t = transcript(&[("▁共", 1.0), ("有しておきます", 0.0)]);
        assert_eq!(punctuate(&t).unwrap(), "共有しておきます。");
    }

    #[test]
    fn without_tokens_only_the_end_is_marked() {
        let t = Transcript {
            text: "明日は会議があります".into(),
            confidence: None,
            tokens: Vec::new(),
        };
        assert_eq!(punctuate(&t).unwrap(), "明日は会議があります。");
    }
}
