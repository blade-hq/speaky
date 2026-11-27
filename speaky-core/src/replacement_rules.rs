use super::{drafts, learned_terms, lock_dictionary, parse, read_vocabulary, Config};
use std::{collections::HashSet, io, ops::Range, path::Path};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Replacement {
    pub original: String,
    pub preferred: String,
}

impl Replacement {
    pub(super) fn new(original: &str, preferred: &str) -> Self {
        Self {
            original: original.into(),
            preferred: preferred.into(),
        }
    }

    pub fn from_pair(original: &str, preferred: &str) -> Option<Self> {
        let (original, preferred) = (original.trim(), preferred.trim());
        (original != preferred && valid_field(original) && valid_field(preferred))
            .then(|| Self::new(original, preferred))
    }

    fn line(&self) -> String {
        format!("{} => {}", self.original, self.preferred)
    }
}

fn valid_field(field: &str) -> bool {
    !field.is_empty()
        && field.trim() == field
        && !field.contains(['\r', '\n', '\0'])
        && !field.contains("=>")
}

pub fn suggest_replacements(
    original: &str,
    edited: &str,
    selection: Option<Range<usize>>,
) -> Vec<Replacement> {
    let suggestions = learned_terms::replacements(original, edited, selection);
    let mut seen = HashSet::new();
    let mut ambiguous = HashSet::new();
    for rule in &suggestions {
        if !seen.insert(rule.original.as_str()) {
            ambiguous.insert(rule.original.as_str());
        }
    }
    suggestions
        .iter()
        .filter(|rule| {
            // Replacing one character globally is too broad, particularly in Chinese.
            rule.original.chars().count() >= 2
                && valid_field(&rule.original)
                && valid_field(&rule.preferred)
                && !ambiguous.contains(rule.original.as_str())
        })
        .cloned()
        .collect()
}

#[derive(Debug)]
pub struct ReplacementUndo {
    changes: Vec<Change>,
}

#[derive(Debug)]
struct Change {
    rule: Replacement,
    previous_lines: Vec<String>,
}

impl ReplacementUndo {
    pub fn count(&self) -> usize {
        self.changes.len()
    }
}

pub fn add_replacements(rules: &[Replacement]) -> io::Result<ReplacementUndo> {
    add_in(&Config::config_dir(), rules)
}

pub fn undo_replacements(undo: &ReplacementUndo) -> io::Result<()> {
    undo_in(&Config::config_dir(), undo)
}

fn preferred_for<'a>(line: &'a str, original: &str) -> Option<&'a str> {
    let (heard, preferred) = line.trim().split_once("=>")?;
    (heard.trim() == original).then(|| preferred.trim())
}

fn add_in(directory: &Path, rules: &[Replacement]) -> io::Result<ReplacementUndo> {
    if rules
        .iter()
        .any(|rule| !valid_field(&rule.original) || !valid_field(&rule.preferred))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "替换规则格式无效。",
        ));
    }
    let incoming = rules
        .iter()
        .map(Replacement::line)
        .collect::<Vec<_>>()
        .join("\n");
    parse(&incoming)?;
    let _lock = lock_dictionary(directory)?;
    let path = directory.join("vocabulary.txt");
    let source = read_vocabulary(&path)?;
    parse(&source)?;
    let mut lines = source.split('\n').map(str::to_owned).collect::<Vec<_>>();
    let mut changes = Vec::new();
    for rule in rules {
        let mut previous_lines = Vec::new();
        let mut exists = false;
        for line in &mut lines {
            if let Some(preferred) = preferred_for(line, &rule.original) {
                exists = true;
                if preferred != rule.preferred {
                    let mut replacement = rule.line();
                    if line.ends_with('\r') {
                        replacement.push('\r');
                    }
                    previous_lines.push(std::mem::replace(line, replacement));
                }
            }
        }
        if !exists {
            let index = lines.len() - usize::from(lines.last().is_some_and(String::is_empty));
            lines.insert(index, rule.line());
        }
        if !exists || !previous_lines.is_empty() {
            changes.push(Change {
                rule: rule.clone(),
                previous_lines,
            });
        }
    }
    if !changes.is_empty() {
        drafts::write_atomically(&path, lines.join("\n").as_bytes())?;
    }
    Ok(ReplacementUndo { changes })
}

fn undo_in(directory: &Path, undo: &ReplacementUndo) -> io::Result<()> {
    let _lock = lock_dictionary(directory)?;
    let path = directory.join("vocabulary.txt");
    let source = read_vocabulary(&path)?;
    parse(&source)?;
    let mut lines = source.split('\n').collect::<Vec<_>>();
    for change in &undo.changes {
        let positions = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                preferred_for(line, &change.rule.original).map(|preferred| (index, preferred))
            })
            .collect::<Vec<_>>();
        if positions.len() != change.previous_lines.len().max(1)
            || positions
                .iter()
                .any(|(_, preferred)| *preferred != change.rule.preferred)
        {
            return Err(io::Error::other(format!(
                "「{}」的替换规则已变更。",
                change.rule.original
            )));
        }
        if change.previous_lines.is_empty() {
            lines.remove(positions[0].0);
        } else {
            for ((index, _), previous) in positions.into_iter().zip(&change.previous_lines) {
                lines[index] = previous;
            }
        }
    }
    drafts::write_atomically(&path, lines.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terms::correct;
    use std::{fs, path::PathBuf};

    struct Dictionary(PathBuf);

    impl Dictionary {
        fn new() -> Self {
            let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, source: &str) {
            fs::write(self.0.join("vocabulary.txt"), source).unwrap();
        }

        fn read(&self) -> String {
            read_vocabulary(&self.0.join("vocabulary.txt")).unwrap()
        }
    }

    impl Drop for Dictionary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn suggests_full_pairs_and_skips_uncertain_rewrites() {
        assert_eq!(
            suggest_replacements("扣德克斯。", "codex", None),
            [Replacement::new("扣德克斯", "codex")]
        );
        assert!(suggest_replacements("请把这几个词全部换掉", "codex", None).is_empty());
        assert_eq!(
            suggest_replacements("小王", "小汪", None),
            [Replacement::new("小王", "小汪")]
        );
        assert_eq!(
            suggest_replacements(
                "我们用 red 开发 agent 工具",
                "我们用 RAG 开发 Agent 工具",
                None
            ),
            [
                Replacement::new("red", "RAG"),
                Replacement::new("agent", "Agent")
            ],
        );
        assert!(suggest_replacements("请小王过来", "请小汪过来", None).is_empty());
        assert_eq!(
            suggest_replacements("请小王过来", "请小汪过来", Some(3..9)),
            [Replacement::new("小王", "小汪")]
        );
        assert!(suggest_replacements("去会议室", "请去操场上集合然后跑步", None).is_empty());
        assert!(
            suggest_replacements("请用 red 或 red 做测试", "请用 RAG 或 RAD 做测试", None)
                .is_empty()
        );
    }

    #[test]
    fn adds_real_replacements_and_undo_restores_overwritten_lines() {
        let dictionary = Dictionary::new();
        let prior = "GPUI\r\n 小王=>小旺 \r\n小王 => 小旺\r\n\r\n";
        dictionary.write(prior);
        let applied = add_in(
            &dictionary.0,
            &[
                Replacement::new("小王", "小汪"),
                Replacement::new("red", "RAG"),
            ],
        )
        .unwrap();
        assert_eq!(applied.count(), 2);
        assert_eq!(
            correct("请小王用 red", &parse(&dictionary.read()).unwrap()),
            "请小汪用 RAG"
        );
        assert_eq!(
            add_in(&dictionary.0, &[Replacement::new("小王", "小汪")])
                .unwrap()
                .count(),
            0
        );
        let _other = add_in(&dictionary.0, &[Replacement::new("扣文", "Qwen")]).unwrap();
        undo_in(&dictionary.0, &applied).unwrap();
        let restored = dictionary.read();
        assert_eq!(restored, format!("{prior}扣文 => Qwen\n"));
        assert_eq!(
            correct("请小王用 red", &parse(&restored).unwrap()),
            "请小旺用 red"
        );
    }

    #[test]
    fn failed_batch_can_retry_and_undo_only_matches_the_saved_values() {
        let dictionary = Dictionary::new();
        dictionary.write("=> 无效规则");
        let rule = Replacement::new("小王", "小汪");
        assert!(add_in(&dictionary.0, std::slice::from_ref(&rule)).is_err());
        assert_eq!(dictionary.read(), "=> 无效规则");
        dictionary.write("Speaky");
        let applied = add_in(&dictionary.0, &[rule]).unwrap();
        assert_eq!(applied.count(), 1);
        dictionary.write("Speaky\n小王 => 王同学");
        assert!(undo_in(&dictionary.0, &applied).is_err());
        assert_eq!(dictionary.read(), "Speaky\n小王 => 王同学");
    }
}
