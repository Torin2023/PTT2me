//! Text stays in memory. Reports contain only counts and rates.
use serde::Serialize;

pub(super) const NORMALIZATION: &str = "ru-lower-yo-alnum-v1";
const MAX_TEXT_CHARACTERS: usize = 2_048;

pub(super) fn normalize(text: &str) -> Result<Vec<String>, String> {
    if text.chars().count() > MAX_TEXT_CHARACTERS {
        return Err("quality text exceeds 2048 Unicode characters".to_owned());
    }
    let normalized: String = text
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| match character {
            'ё' => 'е',
            other if other.is_alphanumeric() => other,
            _ => ' ',
        })
        .collect();
    Ok(normalized.split_whitespace().map(str::to_owned).collect())
}

pub(super) fn validate_reference(reference: &str) -> Result<(), String> {
    if normalize(reference)?.is_empty() {
        return Err("quality reference must contain letters or digits".to_owned());
    }
    Ok(())
}

#[derive(Clone, Default, Serialize)]
pub(super) struct Counts {
    pub(super) word_errors: usize,
    pub(super) reference_words: usize,
    pub(super) character_errors: usize,
    pub(super) reference_characters: usize,
}

#[derive(Serialize)]
pub(super) struct QualityReport {
    #[serde(flatten)]
    pub(super) counts: Counts,
    pub(super) wer: f64,
    pub(super) cer: f64,
}

impl Counts {
    pub(super) fn add(&mut self, other: &Self) {
        self.word_errors += other.word_errors;
        self.reference_words += other.reference_words;
        self.character_errors += other.character_errors;
        self.reference_characters += other.reference_characters;
    }

    pub(super) fn report(&self) -> QualityReport {
        QualityReport {
            counts: self.clone(),
            wer: self.word_errors as f64 / self.reference_words as f64,
            cer: self.character_errors as f64 / self.reference_characters as f64,
        }
    }
}

pub(super) fn score(reference: &str, hypothesis: &str) -> Result<Counts, String> {
    validate_reference(reference)?;
    let reference = normalize(reference)?;
    let hypothesis = normalize(hypothesis)?;
    let reference_characters: Vec<char> = reference.iter().flat_map(|word| word.chars()).collect();
    let hypothesis_characters: Vec<char> =
        hypothesis.iter().flat_map(|word| word.chars()).collect();
    Ok(Counts {
        word_errors: distance(&reference, &hypothesis),
        reference_words: reference.len(),
        character_errors: distance(&reference_characters, &hypothesis_characters),
        reference_characters: reference_characters.len(),
    })
}

fn distance<T: Eq>(left: &[T], right: &[T]) -> usize {
    // Levenshtein distance, one row; no alignment or text enters the report.
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, left_token) in left.iter().enumerate() {
        let mut diagonal = previous[0];
        previous[0] = i + 1;
        for (j, right_token) in right.iter().enumerate() {
            let above = previous[j + 1];
            previous[j + 1] = (above + 1)
                .min(previous[j] + 1)
                .min(diagonal + usize::from(left_token != right_token));
            diagonal = above;
        }
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_policy_is_explicit_and_does_not_expand_numbers() {
        assert_eq!(
            normalize("  Ёж, МОСКВА—тест! 42\n").unwrap(),
            ["еж", "москва", "тест", "42"]
        );
        assert_eq!(score("Ёж идёт", "еж ИДЕТ!").unwrap().word_errors, 0);
        assert_eq!(score("двадцать два", "22").unwrap().word_errors, 2);
        assert_eq!(score("по-русски", "по русски").unwrap().word_errors, 0);
    }

    #[test]
    fn counts_substitutions_insertions_deletions_and_unicode_characters() {
        let counts = score("кот дом", "кит наш дом").unwrap();
        assert_eq!((counts.word_errors, counts.reference_words), (2, 2));
        assert_eq!(
            (counts.character_errors, counts.reference_characters),
            (4, 6)
        );
        let empty = score("кот дом", "").unwrap().report();
        assert_eq!((empty.wer, empty.cer), (1.0, 1.0));
        assert_eq!(score("а", "б в г").unwrap().report().wer, 3.0);
    }

    #[test]
    fn aggregation_is_weighted_by_reference_size_and_contains_no_text() {
        let mut total = score("раз", "два").unwrap();
        total.add(&score("три четыре пять", "три четыре пять").unwrap());
        assert_eq!(total.report().wer, 0.25);
        let json = serde_json::to_string(&total.report()).unwrap();
        assert!(!json.contains("раз"));
        assert!(!json.contains("два"));
        assert!(!json.contains("hypothesis"));
    }

    #[test]
    fn rejects_empty_reference_and_bounds_both_inputs_without_echoing_text() {
        assert!(score("!!!", "текст").is_err());
        let too_long = "я".repeat(2_049);
        for result in [score(&too_long, "а"), score("а", &too_long)] {
            assert_eq!(
                result.err().unwrap(),
                "quality text exceeds 2048 Unicode characters"
            );
        }
        assert!(score(&"а".repeat(2_048), "").is_ok());
    }
}
