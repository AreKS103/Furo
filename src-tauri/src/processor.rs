//! Project Furo — Text Post-Processor (Pure Rust)
//!
//! Three-stage deterministic text refinement — no LLM, no HTTP:
//!   1. Regex symbol dictionary: voiced punctuation → actual symbols
//!   2. Self-correction commands: "scratch that", "I mean" → edits
//!   3. Filler/stutter removal: "um", "uh", duplicated words → clean

use once_cell::sync::Lazy;
use regex::Regex;

// ── Regex Symbol Dictionary

struct SymbolRule {
    pattern: Regex,
    replacement: &'static str,
}

static SYMBOL_RULES: Lazy<Vec<SymbolRule>> = Lazy::new(|| {
    let rules: Vec<(&str, &str)> = vec![
        // Paragraph / line breaks
        (r"(?i)\bnew paragraph\b", "\n\n"),
        (r"(?i)\bnew line\b", "\n"),
        (r"(?i)\bline break\b", "\n"),
        // Punctuation
        (r"(?i)\bperiod\b", "."),
        (r"(?i)\bfull stop\b", "."),
        (r"(?i)\bcomma\b", ","),
        (r"(?i)\bquestion mark\b", "?"),
        (r"(?i)\bexclamation mark\b", "!"),
        (r"(?i)\bexclamation point\b", "!"),
        (r"(?i)\bcolon\b", ":"),
        (r"(?i)\bsemicolon\b", ";"),
        (r"(?i)\bellipsis\b", "…"),
        // Brackets and parens
        (r"(?i)\bopen paren(?:thesis)?\b", "("),
        (r"(?i)\bclose paren(?:thesis)?\b", ")"),
        (r"(?i)\bopen bracket\b", "["),
        (r"(?i)\bclose bracket\b", "]"),
        (r"(?i)\bopen brace\b", "{"),
        (r"(?i)\bclose brace\b", "}"),
        // Quotes
        (r"(?i)\bopen quote\b", "\""),
        (r"(?i)\bclose quote\b", "\""),
        (r"(?i)\bsingle quote\b", "'"),
        // Math / programming
        (r"(?i)\bplus sign\b", "+"),
        (r"(?i)\bminus sign\b", "-"),
        (r"(?i)\bequals sign\b", "="),
        (r"(?i)\basterisk\b", "*"),
        (r"(?i)\bforward slash\b", "/"),
        (r"(?i)\bbackslash\b", "\\"),
        (r"(?i)\bampersand\b", "&"),
        (r"(?i)\bat sign\b", "@"),
        (r"(?i)\bdollar sign\b", "$"),
        (r"(?i)\bpercent sign\b", "%"),
        (r"(?i)\btilde\b", "~"),
        (r"(?i)\bcaret\b", "^"),
        (r"(?i)\bunderscore\b", "_"),
        // Special
        (r"(?i)\bhyphen\b", "-"),
        (r"(?i)\bem dash\b", "—"),
        (r"(?i)\ben dash\b", "–"),
        (r"(?i)\btab\b", "\t"),
    ];

    rules
        .into_iter()
        .map(|(pat, rep)| SymbolRule {
            pattern: Regex::new(pat).unwrap(),
            replacement: rep,
        })
        .collect()
});

static SPACE_BEFORE_PUNCT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r" +([.,;:!?\)\]\}])").unwrap());

fn apply_symbol_dict(text: &str) -> String {
    let mut result = text.to_string();
    for rule in SYMBOL_RULES.iter() {
        result = rule.pattern.replace_all(&result, rule.replacement).to_string();
    }
    result = SPACE_BEFORE_PUNCT.replace_all(&result, "$1").to_string();
    result.trim().to_string()
}

// ── Filler & Self-Correction Rules

static MULTI_SPACE: Lazy<Regex> = Lazy::new(|| Regex::new(r"  +").unwrap());

static SPEECH_RULES: Lazy<Vec<SymbolRule>> = Lazy::new(|| {
    let rules: Vec<(&str, &str)> = vec![
        // Self-correction "scratch that" at END (no replacement): delete from the
        // mistaken word(s) onwards — up to 5 words before the marker.
        // "reduces scratch that" → "" so preceding context is preserved.
        (r"(?i)\s+\w[\w'-]*(?:\s+\w[\w'-]*){0,4}[,\s]*\b(?:scratch|strike|delete|forget)\s+that\b\s*[.!?]?\s*$", ""),
        // Self-correction "scratch that" MID-sentence: delete the mistaken word(s)
        // + the marker — keep everything before AND after.
        // "prompt reduces, scratch that, increases" → "prompt increases"
        (r"(?i)\b\w[\w'-]*(?:\s+\w[\w'-]*){0,4}[,\s]+(?:scratch|strike|delete|forget)\s+that[,\s]+", ""),
        // Self-correction "I mean / correction / or rather": delete only the
        // immediately preceding word + the marker. Whisper may add a newline
        // between the marker and the correction, so use \s+ to absorb it.
        // "reduces, I mean\n  increases" → "increases" (with context before intact)
        (r"(?i)\b\w[\w'-]*[,\s]+(?:I\s+mean|correction[,:]?|or\s+rather)[,\s]+", ""),
        // Self-correction "no wait / wait no": remove just the spoken marker
        (r"(?i)[,\s]*\b(?:no\s+wait|wait\s+no)\b[,\s]*", " "),
        // Filler vocalizations: um, uh, er, ah, eh, hmm, hm
        (r"(?i)\b(?:um+|uh+|er+|ah|eh|hmm+|hm)\b[,]?\s*", " "),
        // "you know" as a filler phrase
        (r"(?i)\byou\s+know(?:\s+what\s+I\s+mean)?\b[,]?\s*", " "),
    ];
    rules
        .into_iter()
        .map(|(pat, rep)| SymbolRule {
            pattern: Regex::new(pat).unwrap(),
            replacement: rep,
        })
        .collect()
});

/// Remove stuttered word repetitions (e.g. "I I want" → "I want").
/// Implemented without regex backreferences (unsupported in Rust's regex crate).
fn remove_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        result.push(words[i]);
        if i + 1 < words.len()
            && words[i].to_lowercase() == words[i + 1].to_lowercase()
        {
            i += 2; // skip the duplicate
        } else {
            i += 1;
        }
    }
    result.join(" ")
}

fn apply_speech_rules(text: &str) -> String {
    let mut result = text.to_string();
    for rule in SPEECH_RULES.iter() {
        result = rule.pattern.replace_all(&result, rule.replacement).to_string();
    }
    result = remove_stutters(&result);
    result = MULTI_SPACE.replace_all(&result, " ").to_string();
    result.trim().to_string()
}

/// Process raw Whisper output through all text cleaning stages.
/// Purely deterministic regex transforms — no LLM, no HTTP.
pub fn process(text: &str) -> String {
    // Stage 1: voiced punctuation (regex symbol dictionary)
    let text = apply_symbol_dict(text);
    if text.is_empty() {
        return String::new();
    }

    // Stage 2: filler removal and self-corrections
    let text = apply_speech_rules(&text);
    if text.is_empty() {
        return String::new();
    }

    log::info!("Processor: \"{}\"", text);
    text
}
