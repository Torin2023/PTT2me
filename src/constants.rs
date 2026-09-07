// Storage and ASR wire bounds share this allowance for delayed capture stop.
pub const CAPTURE_BUFFER_MARGIN_MS: u64 = 1_000;
pub const SAMPLE_RATE: u32 = 16_000;
pub const RELEASE_GRACE_MS: u64 = 180;
pub const MAX_CAPTURE_MS: u64 = 25_000;
pub const ERROR_VISIBLE_MS: u64 = 3_000;
pub const BUNDLE_ID: &str = "com.ptt2me.app";
pub const MAX_UPDATE_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_UPDATE_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
pub const UPDATE_CONNECT_TIMEOUT_SECONDS: u64 = 10;
pub const UPDATE_READ_TIMEOUT_SECONDS: u64 = 30;
pub const UPDATE_OVERALL_TIMEOUT_SECONDS: u64 = 15 * 60;
pub const UPDATE_MAX_REDIRECTS: u32 = 5;
