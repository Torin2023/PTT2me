use std::time::Duration;

pub(crate) const AUDIO_PREPARATION: &str = "audio_preparation";
pub(crate) const ASR_WORKER_LOAD: &str = "asr_worker_load_round_trip";
pub(crate) const ASR_WORKER_TRANSCRIPTION: &str = "asr_worker_transcription_round_trip";
pub(crate) const INSERTION_PREPARATION: &str = "insertion_target_snapshot_preparation";
pub(crate) const INSERTION_SECURITY_PROBE: &str = "insertion_pre_command_v_security_probe";
pub(crate) const CLIPBOARD_RESTORATION: &str = "clipboard_restoration";

pub(crate) fn log(phase: &'static str, elapsed: Duration, outcome: &'static str) {
    tracing::debug!(phase, elapsed_ms = elapsed.as_millis() as u64, outcome);
}

#[cfg(test)]
mod tests {
    #[test]
    fn phase_diagnostics_have_fixed_privacy_safe_names() {
        assert_eq!(
            [
                super::AUDIO_PREPARATION,
                super::ASR_WORKER_LOAD,
                super::ASR_WORKER_TRANSCRIPTION,
                super::INSERTION_PREPARATION,
                super::INSERTION_SECURITY_PROBE,
                super::CLIPBOARD_RESTORATION,
            ],
            [
                "audio_preparation",
                "asr_worker_load_round_trip",
                "asr_worker_transcription_round_trip",
                "insertion_target_snapshot_preparation",
                "insertion_pre_command_v_security_probe",
                "clipboard_restoration",
            ]
        );
    }
}
