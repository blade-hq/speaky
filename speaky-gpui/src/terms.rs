use super::{config::Config, engines};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
};

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
    fs::create_dir_all(Config::config_dir())?;
    let path = vocabulary_path();
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
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
) -> engines::EngineResult {
    let mut source = String::new();
    let rules = match load()
        .map(|loaded| source = loaded)
        .and_then(|_| parse(&source))
    {
        Ok(rules) => rules,
        Err(error) => {
            log::warn!("Could not read Speaky vocabulary: {error}");
            return engine.transcribe(audio, language);
        }
    };
    let mut vocabulary = Vec::new();
    for rule in &rules {
        if !vocabulary.contains(&rule.preferred) {
            vocabulary.push(rule.preferred);
        }
    }
    let transcript = engine.transcribe_with_vocabulary(audio, language, &vocabulary)?;
    Ok(correct(&transcript, &rules))
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
}
