use super::{config::Config, drafts, engines};
use std::{
    fs, io,
    ops::Range,
    path::{Path, PathBuf},
};

#[path = "learned_terms.rs"]
mod learned_terms;

fn lock_dictionary(directory: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(directory)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("vocabulary.lock"))?;
    file.lock()?;
    Ok(file)
}

struct Rule<'a> {
    heard: Option<&'a str>,
    preferred: &'a str,
}

fn vocabulary_path() -> PathBuf {
    Config::config_dir().join("vocabulary.txt")
}

pub fn load() -> io::Result<String> {
    match fs::read_to_string(vocabulary_path()) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        result => result,
    }
}

pub fn save(source: &str) -> io::Result<()> {
    parse(source)?;
    let _lock = lock_dictionary(&Config::config_dir())?;
    drafts::write_atomically(&vocabulary_path(), source.as_bytes())
}

fn learned_path() -> PathBuf {
    Config::config_dir().join("learned-vocabulary.json")
}

fn read_learned(path: &Path) -> io::Result<Vec<String>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn write_learned(path: &Path, words: &[String]) -> io::Result<()> {
    drafts::write_atomically(path, &serde_json::to_vec(words).map_err(io::Error::other)?)
}

fn manual_has(rules: &[Rule<'_>], term: &str) -> bool {
    rules
        .iter()
        .any(|rule| rule.preferred.eq_ignore_ascii_case(term))
}

pub(crate) fn learned() -> io::Result<Vec<String>> {
    let mut words = read_learned(&learned_path())?;
    let source = load()?;
    let manual = parse(&source)?;
    words.retain(|word| !manual_has(&manual, word));
    Ok(words)
}

pub(crate) fn learn_edits_with_selection(
    original: &str,
    edited: &str,
    selection: Option<Range<usize>>,
) -> io::Result<Vec<String>> {
    let changes = learned_terms::replacements(original, edited, selection);
    if changes.is_empty() {
        return Ok(changes);
    }
    let _lock = lock_dictionary(&Config::config_dir())?;
    let source = load()?;
    remember(&learned_path(), &parse(&source)?, changes)
}

fn remember(path: &Path, manual: &[Rule<'_>], changes: Vec<String>) -> io::Result<Vec<String>> {
    let mut words = read_learned(path)?;
    let mut added = Vec::new();
    for term in changes {
        if manual_has(manual, &term) || words.contains(&term) {
            continue;
        }
        words.retain(|old| !old.eq_ignore_ascii_case(&term));
        words.insert(0, term.clone());
        added.push(term);
    }
    if !added.is_empty() {
        write_learned(path, &words)?;
    }
    Ok(added)
}

pub(crate) fn forget_learned(term: &str) -> io::Result<()> {
    let _lock = lock_dictionary(&Config::config_dir())?;
    let path = learned_path();
    let mut words = read_learned(&path)?;
    if let Some(index) = words.iter().position(|word| word == term) {
        words.remove(index);
        write_learned(&path, &words)?;
    }
    Ok(())
}

fn parse(source: &str) -> io::Result<Vec<Rule<'_>>> {
    let mut rules: Vec<Rule<'_>> = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (heard, preferred) = match line.split_once("=>") {
            Some((heard, preferred)) => (Some(heard.trim()), preferred.trim()),
            None => (None, line),
        };
        if heard == Some("") || preferred.is_empty() || preferred.contains("=>") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("第 {} 行需要填写“误识别词 => 正确写法”。", index + 1),
            ));
        }
        if rules
            .iter()
            .any(|rule| heard.is_some() && rule.heard == heard && rule.preferred != preferred)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("第 {} 行与前面的改写规则冲突。", index + 1),
            ));
        }
        rules.push(Rule { heard, preferred });
    }
    Ok(rules)
}

fn correct(text: &str, rules: &[Rule<'_>]) -> String {
    let mut replacements = rules
        .iter()
        .filter_map(|rule| Some((rule.heard?, rule.preferred)))
        .collect::<Vec<_>>();
    if replacements.is_empty() {
        return text.to_owned();
    }
    replacements.sort_by_key(|(heard, _)| std::cmp::Reverse(heard.len()));
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while !remaining.is_empty() {
        let next = replacements
            .iter()
            .filter_map(|&(heard, preferred)| {
                let position = remaining.find(heard)?;
                Some((position, heard.len(), preferred))
            })
            .min_by_key(|(position, _, _)| *position);
        let Some((position, length, preferred)) = next else {
            result.push_str(remaining);
            break;
        };
        result.push_str(&remaining[..position]);
        result.push_str(preferred);
        remaining = &remaining[position + length..];
    }
    result
}

pub fn transcribe(
    engine: &dyn engines::Engine,
    audio: &[u8],
    language: &str,
    pass: engines::TranscriptionPass,
) -> engines::EngineResult {
    let mut source = String::new();
    let rules = match load()
        .map(|loaded| source = loaded)
        .and_then(|_| parse(&source))
    {
        Ok(rules) => rules,
        Err(error) => {
            log::warn!("Could not read Speaky vocabulary: {error}");
            Vec::new()
        }
    };
    let learned = read_learned(&learned_path()).unwrap_or_else(|error| {
        log::warn!("Could not read learned vocabulary: {error}");
        Vec::new()
    });
    let vocabulary = hints(&rules, &learned);
    let transcript = engine.transcribe_with_vocabulary(audio, language, &vocabulary, pass)?;
    Ok(correct(&transcript, &rules))
}

fn hints<'a>(manual: &[Rule<'a>], learned: &'a [String]) -> Vec<&'a str> {
    let mut seen = std::collections::HashSet::new();
    let mut typed = manual.iter().map(|rule| rule.preferred);
    let mut recent = learned
        .iter()
        .map(String::as_str)
        .filter(|word| !manual_has(manual, word));
    let mut result = Vec::new();
    // The model's prompt has a byte limit. Interleave recent corrections and
    // manual entries so either source can contribute when the other is long.
    loop {
        let pair = [recent.next(), typed.next()];
        if pair == [None, None] {
            break;
        }
        for value in pair.into_iter().flatten() {
            if seen.insert(value) {
                result.push(value);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{correct, parse};

    #[test]
    fn applies_longest_matching_rule_without_chaining() {
        let rules =
            parse("说话 => Speaky\n说话软件 => Speaky Dictation\nSpeaky => Other\nGPUI").unwrap();
        assert_eq!(
            correct("用说话软件测试，说话正常。", &rules),
            "用Speaky Dictation测试，Speaky正常。"
        );
    }

    #[test]
    fn validates_rules_without_rejecting_vocabulary_only() {
        assert!(parse("GPUI\nSpeaky").is_ok());
        assert!(parse("=> 空").is_err());
        assert!(parse("空 =>").is_err());
        assert!(parse("同音 => 专名\n同音 => 另一个词").is_err());
    }

    #[test]
    fn independent_dictionary_writers_keep_both_corrections() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = root.join("learned-vocabulary.json");
        let gate = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for word in ["RAG", "Speaky"] {
                let (root, path, gate) = (&root, &path, &gate);
                scope.spawn(move || {
                    gate.wait();
                    let _guard = super::lock_dictionary(root).unwrap();
                    super::remember(path, &[], vec![word.to_owned()]).unwrap();
                });
            }
        });
        let mut words = super::read_learned(&path).unwrap();
        words.sort();
        assert_eq!(words, ["RAG", "Speaky"]);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(root.join("vocabulary.lock")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn persists_learned_spelling_once_and_uses_it_only_as_a_hint() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let path = root.join("learned-vocabulary.json");
        let manual = parse("扣文 => Qwen\nSpeaky").unwrap();
        assert_eq!(
            super::remember(&path, &manual, vec!["RAG".into(), "Qwen".into()]).unwrap(),
            ["RAG"]
        );
        assert!(super::remember(&path, &manual, vec!["RAG".into()])
            .unwrap()
            .is_empty());
        let learned = super::read_learned(&path).unwrap();
        assert_eq!(super::hints(&manual, &learned), ["RAG", "Qwen", "Speaky"]);
        assert_eq!(correct("red 扣文", &manual), "red Qwen");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
