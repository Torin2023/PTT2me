use std::ffi::OsString;
use std::path::PathBuf;

const MAX_WARMUPS: u32 = 3;
const MAX_REPEATS: u32 = 20;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Arguments {
    pub(super) model: PathBuf,
    pub(super) corpus: PathBuf,
    pub(super) threads: Vec<i32>,
    pub(super) warmups: u32,
    pub(super) repeats: u32,
}

pub(super) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut arguments = arguments.into_iter();
    let mut model = None;
    let mut corpus = None;
    let mut consent = false;
    let mut threads = vec![1, 2, 4];
    let mut warmups = 1;
    let mut repeats = 5;
    let mut threads_set = false;
    let mut warmups_set = false;
    let mut repeats_set = false;

    while let Some(argument) = arguments.next() {
        let option = argument
            .to_str()
            .ok_or_else(|| "option name is not valid UTF-8".to_owned())?;
        match option {
            "--model" if model.is_none() => model = Some(required_path(&mut arguments, option)?),
            "--corpus" if corpus.is_none() => corpus = Some(required_path(&mut arguments, option)?),
            "--consent-to-process-audio" if !consent => consent = true,
            "--threads" if !threads_set => {
                threads = parse_threads(&required_utf8(&mut arguments, option)?)?;
                threads_set = true;
            }
            "--warmups" if !warmups_set => {
                warmups = parse_count(
                    "warmups",
                    &required_utf8(&mut arguments, option)?,
                    1,
                    MAX_WARMUPS,
                )?;
                warmups_set = true;
            }
            "--repeats" if !repeats_set => {
                repeats = parse_count(
                    "repeats",
                    &required_utf8(&mut arguments, option)?,
                    1,
                    MAX_REPEATS,
                )?;
                repeats_set = true;
            }
            _ => return Err(format!("unknown or repeated option: {option}")),
        }
    }

    if !consent {
        return Err("--consent-to-process-audio is required".to_owned());
    }
    Ok(Arguments {
        model: model.ok_or_else(|| "--model is required".to_owned())?,
        corpus: corpus.ok_or_else(|| "--corpus is required".to_owned())?,
        threads,
        warmups,
        repeats,
    })
}

fn required_path(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(
        arguments
            .next()
            .ok_or_else(|| format!("{option} requires a value"))?,
    );
    if path.as_os_str().is_empty() {
        Err(format!("{option} requires a non-empty path"))
    } else {
        Ok(path)
    }
}

fn required_utf8(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))?
        .into_string()
        .map_err(|_| format!("{option} value is not valid UTF-8"))
}

fn parse_threads(raw: &str) -> Result<Vec<i32>, String> {
    let mut parsed = Vec::new();
    for value in raw.split(',') {
        let value = value
            .parse::<i32>()
            .map_err(|_| format!("invalid thread count: {value}"))?;
        if ![1, 2, 4].contains(&value) || parsed.contains(&value) {
            return Err("threads must be a unique comma-separated subset of 1,2,4".to_owned());
        }
        parsed.push(value);
    }
    if parsed.is_empty() {
        Err("at least one thread count is required".to_owned())
    } else {
        Ok(parsed)
    }
}

fn parse_count(name: &str, raw: &str, minimum: u32, maximum: u32) -> Result<u32, String> {
    let count = raw
        .parse::<u32>()
        .map_err(|_| format!("invalid {name}: {raw}"))?;
    if (minimum..=maximum).contains(&count) {
        Ok(count)
    } else {
        Err(format!("{name} must be in {minimum}..={maximum}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_arguments() -> Vec<OsString> {
        [
            "--model",
            "/tmp/model",
            "--corpus",
            "/tmp/corpus.json",
            "--consent-to-process-audio",
            "--threads",
            "1,2,4",
            "--warmups",
            "1",
            "--repeats",
            "5",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn accepts_explicit_bounded_arguments() {
        assert_eq!(
            parse(valid_arguments()).unwrap(),
            Arguments {
                model: PathBuf::from("/tmp/model"),
                corpus: PathBuf::from("/tmp/corpus.json"),
                threads: vec![1, 2, 4],
                warmups: 1,
                repeats: 5,
            }
        );
    }

    #[test]
    fn rejects_missing_consent_unknown_options_and_unbounded_counts() {
        let mut missing_consent = valid_arguments();
        missing_consent.retain(|value| value != "--consent-to-process-audio");
        assert!(parse(missing_consent).is_err());

        for replacement in ["0", "21"] {
            let mut arguments = valid_arguments();
            let index = arguments.iter().position(|value| value == "5").unwrap();
            arguments[index] = replacement.into();
            assert!(parse(arguments).is_err());
        }

        let mut unknown = valid_arguments();
        unknown.push("--download".into());
        assert!(parse(unknown).is_err());

        let mut repeated = valid_arguments();
        repeated.extend([OsString::from("--model"), OsString::from("/tmp/other")]);
        assert!(parse(repeated).is_err());
    }

    #[test]
    fn accepts_only_unique_supported_cpu_thread_counts() {
        for value in ["0", "3", "8", "1,1", "1,2,3"] {
            let mut arguments = valid_arguments();
            let index = arguments.iter().position(|item| item == "1,2,4").unwrap();
            arguments[index] = value.into();
            assert!(parse(arguments).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn limits_warmups_and_repeats() {
        assert_eq!(MAX_WARMUPS, 3);
        assert_eq!(MAX_REPEATS, 20);
    }
}
