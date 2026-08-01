use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModelPaths {
    encoder: PathBuf,
    decoder: PathBuf,
    joiner: PathBuf,
    tokens: PathBuf,
}

impl ModelPaths {
    pub(crate) fn from_verified_directory(model_dir: &Path) -> Self {
        Self {
            encoder: model_dir.join("encoder.int8.onnx"),
            decoder: model_dir.join("decoder.onnx"),
            joiner: model_dir.join("joiner.onnx"),
            tokens: model_dir.join("tokens.txt"),
        }
    }

    pub fn encoder(&self) -> &Path {
        &self.encoder
    }

    pub fn decoder(&self) -> &Path {
        &self.decoder
    }

    pub fn joiner(&self) -> &Path {
        &self.joiner
    }

    pub fn tokens(&self) -> &Path {
        &self.tokens
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        encoder: PathBuf,
        decoder: PathBuf,
        joiner: PathBuf,
        tokens: PathBuf,
    ) -> Self {
        Self {
            encoder,
            decoder,
            joiner,
            tokens,
        }
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::resources_dir_from_executable;

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
