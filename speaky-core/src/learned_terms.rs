//! Find short, reviewable replacements in an edited transcript.
use super::Replacement;
use std::ops::Range;

fn han(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af)
}

fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut chars = text.char_indices().peekable();
    let mut words = Vec::new();
    while let Some((start, ch)) = chars.next() {
        if han(ch) {
            words.push(start..start + ch.len_utf8());
            continue;
        }
        let dot_prefix = ch == '.' && chars.peek().is_some_and(|(_, c)| c.is_ascii_alphabetic());
        if !ch.is_alphanumeric() && !dot_prefix {
            continue;
        }
        let mut end = start + ch.len_utf8();
        while let Some(&(p, c)) = chars.peek() {
            let word_char = c.is_alphanumeric() && !han(c);
            let connector = matches!(c, '.' | '-' | '_' | '/' | '\'' | '’')
                && text[p + c.len_utf8()..].starts_with(|n: char| n.is_ascii_alphanumeric());
            if !(word_char || connector || matches!(c, '#' | '+')) {
                break;
            }
            chars.next();
            end = p + c.len_utf8();
        }
        words.push(start..end);
    }
    words
}

fn span<'a>(text: &'a str, words: &[Range<usize>]) -> &'a str {
    match (words.first(), words.last()) {
        (Some(first), Some(last)) => &text[first.start..last.end],
        _ => "",
    }
}

fn valid_term(preferred: &str) -> bool {
    if !(2..=32).contains(&preferred.chars().count())
        || !preferred.chars().any(char::is_alphabetic)
        || !preferred
            .chars()
            .all(|c| c.is_alphanumeric() || " ._-/#+'’".contains(c))
    {
        return false;
    }
    let words = tokens(preferred);
    if preferred.chars().all(han) {
        return words.len() <= 5
            && !matches!(preferred, "那个" | "这个" | "就是" | "然后" | "嗯嗯");
    }
    if words.len() == 1 {
        return true;
    }
    if preferred.contains(han) {
        return !preferred.contains(' ')
            && preferred.chars().filter(|c| han(*c)).count() <= 3
            && preferred.contains(|c: char| c.is_ascii_uppercase());
    }
    // A multi-word proper name such as Visual Studio can be a term. Ordinary
    // prose edits spanning several words should stay in the transcript.
    words.len() <= 4
        && words.iter().all(|w| {
            let part = &preferred[w.clone()];
            part.contains(|c: char| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
}

fn candidate(heard: &str, preferred: &str) -> bool {
    !heard.is_empty() && heard != preferred && heard.chars().count() <= 40 && valid_term(preferred)
}

// Extend a changed span to an explicitly selected term only if the added
// context matches on both sides of the edit.
fn selected_original<'a>(
    original: &'a str,
    edited: &str,
    old: &[Range<usize>],
    new: &[Range<usize>],
    before: Range<usize>,
    after: Range<usize>,
    selection: &Range<usize>,
) -> Option<&'a str> {
    let first = new.iter().position(|word| word.start == selection.start)?;
    let last = new.iter().position(|word| word.end == selection.end)? + 1;
    let start = before.start.checked_sub(after.start.checked_sub(first)?)?;
    let end = before.end + last.checked_sub(after.end)?;
    let (start, end) = (old.get(start)?.start, old.get(end - 1)?.end);
    let (changed_start, changed_end) = (old[before.start].start, old[before.end - 1].end);
    if original[start..changed_start] != edited[selection.start..new[after.start].start]
        || original[changed_end..end] != edited[new[after.end - 1].end..selection.end]
    {
        return None;
    }
    Some(&original[start..end])
}

pub(super) fn replacements(
    original: &str,
    edited: &str,
    selection: Option<Range<usize>>,
) -> Vec<Replacement> {
    let old = tokens(original);
    let new = tokens(edited);
    if old.is_empty() || new.is_empty() {
        return Vec::new();
    }
    let equal = |i: usize, j: usize| original[old[i].clone()] == edited[new[j].clone()];
    let prefix = (0..old.len().min(new.len()))
        .take_while(|&i| equal(i, i))
        .count();
    let suffix = (0..(old.len() - prefix).min(new.len() - prefix))
        .take_while(|&i| equal(old.len() - 1 - i, new.len() - 1 - i))
        .count();
    let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
    let (n, m) = (old_end - prefix, new_end - prefix);
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let selection = selection.filter(|range| {
        let Some(word) = edited.get(range.clone()) else {
            return false;
        };
        valid_term(word)
            && new.iter().any(|token| token.start == range.start)
            && new.iter().any(|token| token.end == range.end)
    });
    // Bound both time and memory for a wholesale rewrite of a long recording.
    if n.saturating_mul(m) > 250_000 {
        return Vec::new();
    }
    let width = m + 1;
    let mut lengths = vec![0usize; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lengths[i * width + j] = if equal(prefix + i, prefix + j) {
                1 + lengths[(i + 1) * width + j + 1]
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    let unchanged = prefix + suffix + lengths[0];
    let whole_old = span(original, &old);
    let whole_new = span(edited, &new);
    let standalone = (new.len() == 1
        && (old.len() == 1
            || (old.len() <= 4
                && whole_old.chars().count() <= 14
                && (whole_new.contains(|c: char| c.is_ascii_uppercase())
                    || (whole_old.chars().all(han)
                        && whole_new.contains(|c: char| c.is_ascii_alphabetic()))))))
        || (unchanged > 0
            && whole_old.chars().all(han)
            && whole_new.chars().all(han)
            && (2..=4).contains(&old.len())
            && (2..=4).contains(&new.len()));
    // For an isolated Chinese name the boundaries are known even if only
    // the final character changes (e.g. 张为 -> 张伟).
    if selection.is_none() && standalone && candidate(whole_old, whole_new) {
        return vec![Replacement::new(whole_old, whole_new)];
    }
    let local = unchanged > 0 && unchanged * 2 >= old.len().max(new.len());
    if !local && selection.is_none() {
        return Vec::new();
    }
    let mut added = Vec::new();
    let mut collect = |before: Range<usize>, after: Range<usize>| {
        if before.is_empty() || after.is_empty() {
            return;
        }
        let selected = selection.as_ref().and_then(|range| {
            selected_original(
                original,
                edited,
                &old,
                &new,
                before.clone(),
                after.clone(),
                range,
            )
            .map(|heard| (heard, &edited[range.clone()]))
        });
        let (heard, preferred) = if let Some(pair) = selected {
            pair
        } else if local || standalone {
            (span(original, &old[before]), span(edited, &new[after]))
        } else {
            return;
        };
        if candidate(heard, preferred) {
            let rule = Replacement::new(heard, preferred);
            if !added.contains(&rule) {
                added.push(rule);
            }
        }
    };
    let (mut i, mut j) = (0, 0);
    let (mut before, mut after) = (prefix, prefix);
    while i < n && j < m {
        if equal(prefix + i, prefix + j) {
            collect(before..prefix + i, after..prefix + j);
            i += 1;
            j += 1;
            before = prefix + i;
            after = prefix + j;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    collect(before..old_end, after..new_end);
    added
}

#[cfg(test)]
mod tests {
    fn replacements(original: &str, edited: &str) -> Vec<String> {
        super::replacements(original, edited, None)
            .into_iter()
            .map(|rule| rule.preferred)
            .collect()
    }
    #[test]
    fn isolates_multiple_spelling_changes_in_mixed_dictation() {
        assert_eq!(
            replacements("我们用 red 开发 agent 工具", "我们用 RAG 开发 Agent 工具"),
            ["RAG", "Agent"]
        );
        assert_eq!(
            replacements("今天用扣文做实验", "今天用Qwen3-ASR做实验"),
            ["Qwen3-ASR"]
        );
        assert_eq!(
            replacements("这是 nodejs 服务", "这是 Node.js 服务"),
            ["Node.js"]
        );
        assert_eq!(replacements("请联系王五开会", "请联系赵敏开会"), ["赵敏"]);
        assert_eq!(replacements("张为。", "张伟。"), ["张伟"]);
        assert_eq!(replacements("艾振特", "Agent"), ["Agent"]);
        assert_eq!(replacements("R A G", "RAG"), ["RAG"]);
    }
    #[test]
    fn ignores_insertions_deletions_formatting_and_rewrites() {
        for (old, new) in [
            ("来开会", "我们来开会"),
            ("啊来开会", "来开会"),
            ("说 RAG 吧", "说RAG，吧！"),
            ("去会议室", "请去操场上集合然后跑步"),
            ("hello world today", "replace this sentence completely"),
        ] {
            assert!(replacements(old, new).is_empty(), "{old} -> {new}");
        }
    }
    #[test]
    fn explicit_selection_must_cover_the_edited_word() {
        let src = "参会的是张为老师，与张伟同名";
        let out = "参会的是张伟老师，与张伟同名";
        let selected = out.find("张伟").unwrap();
        assert_eq!(
            super::replacements(src, out, Some(selected..selected + "张伟".len())),
            [super::Replacement::new("张为", "张伟")]
        );
        let unchanged = out.rfind("张伟").unwrap();
        assert!(
            super::replacements(src, out, Some(unchanged..unchanged + "张伟".len())).is_empty()
        );
        assert!(super::replacements(src, out, Some(0..out.len())).is_empty());
    }
}
