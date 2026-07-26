use std::path::{Path, PathBuf};

const MODEL_DIRECTORY: &str = "models/gigaam-v3-rnnt";

#[derive(Debug, Clone)]
pub struct ModelPaths {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

impl ModelPaths {
    pub fn from_resources(resources_dir: &Path) -> Result<Self, String> {
        let model_dir = resources_dir.join(MODEL_DIRECTORY);
        Ok(Self {
            encoder: required_file(&model_dir, "encoder", "encoder.int8.onnx")?,
            decoder: required_file(&model_dir, "decoder", "decoder.onnx")?,
            joiner: required_file(&model_dir, "joiner", "joiner.onnx")?,
            tokens: required_file(&model_dir, "tokens", "tokens.txt")?,
        })
    }
}

pub fn resources_dir_from_executable(executable: &Path) -> Result<PathBuf, String> {
    let macos_dir = executable
        .parent()
        .ok_or_else(|| format!("invalid executable path: {}", executable.display()))?;
    let contents_dir = macos_dir
        .parent()
        .ok_or_else(|| format!("invalid executable path: {}", executable.display()))?;
    Ok(contents_dir.join("Resources"))
}

fn required_file(model_dir: &Path, role: &str, filename: &str) -> Result<PathBuf, String> {
    let path = model_dir.join(filename);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "missing bundled {role} model file: {}",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::{resources_dir_from_executable, ModelPaths};

    const MODEL_DIR: &str = "models/gigaam-v3-rnnt";

    fn write_fixture_files(temp: &TempDir, files: &[&str]) {
        let model_dir = temp.path().join(MODEL_DIR);
        fs::create_dir_all(&model_dir).unwrap();
        for file in files {
            fs::write(model_dir.join(file), []).unwrap();
        }
    }

    fn all_files() -> [&'static str; 4] {
        [
            "encoder.int8.onnx",
            "decoder.onnx",
            "joiner.onnx",
            "tokens.txt",
        ]
    }

    #[test]
    fn resolves_all_model_paths_from_bundled_resources() {
        let temp = TempDir::new().unwrap();
        write_fixture_files(&temp, &all_files());

        let paths = ModelPaths::from_resources(temp.path()).unwrap();

        assert!(paths
            .encoder
            .ends_with("models/gigaam-v3-rnnt/encoder.int8.onnx"));
        assert!(paths
            .decoder
            .ends_with("models/gigaam-v3-rnnt/decoder.onnx"));
        assert!(paths.joiner.ends_with("models/gigaam-v3-rnnt/joiner.onnx"));
        assert!(paths.tokens.ends_with("models/gigaam-v3-rnnt/tokens.txt"));
    }

    #[test]
    fn rejects_missing_encoder() {
        let temp = TempDir::new().unwrap();
        write_fixture_files(&temp, &["decoder.onnx", "joiner.onnx", "tokens.txt"]);

        assert!(ModelPaths::from_resources(temp.path())
            .unwrap_err()
            .contains("encoder"));
    }

    #[test]
    fn rejects_missing_decoder() {
        let temp = TempDir::new().unwrap();
        write_fixture_files(&temp, &["encoder.int8.onnx", "joiner.onnx", "tokens.txt"]);

        assert!(ModelPaths::from_resources(temp.path())
            .unwrap_err()
            .contains("decoder"));
    }

    #[test]
    fn rejects_missing_joiner() {
        let temp = TempDir::new().unwrap();
        write_fixture_files(&temp, &["encoder.int8.onnx", "decoder.onnx", "tokens.txt"]);

        assert!(ModelPaths::from_resources(temp.path())
            .unwrap_err()
            .contains("joiner"));
    }

    #[test]
    fn rejects_missing_tokens() {
        let temp = TempDir::new().unwrap();
        write_fixture_files(&temp, &["encoder.int8.onnx", "decoder.onnx", "joiner.onnx"]);

        assert!(ModelPaths::from_resources(temp.path())
            .unwrap_err()
            .contains("tokens"));
    }

    #[test]
    fn finds_resources_directory_from_app_executable() {
        assert_eq!(
            resources_dir_from_executable(Path::new(
                "/Applications/PTT2me.app/Contents/MacOS/PTT2me"
            ))
            .unwrap(),
            PathBuf::from("/Applications/PTT2me.app/Contents/Resources")
        );
    }
}
