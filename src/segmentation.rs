pub const DEFAULT_SEGMENT_TARGET_MS: u64 = 10_000;
pub const DEFAULT_SEGMENT_GRACE_MS: u64 = 2_000;
pub const DEFAULT_SEGMENT_OVERLAP_MS: u64 = 400;
pub const DEFAULT_SEGMENT_MIN_MS: u64 = 1_200;

#[derive(Debug, Clone, PartialEq)]
pub struct SegmentConfig {
    pub sample_rate: u32,
    pub vad_threshold: f32,
    pub silence_samples: usize,
    pub target_samples: usize,
    pub grace_samples: usize,
    pub overlap_samples: usize,
    pub min_segment_samples: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CutReason {
    Silence,
    SoftLimitPause,
    HardLimit,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SegmentDecision {
    Continue,
    Cut {
        speech_end: usize,
        reason: CutReason,
    },
}

pub fn decide_segment(
    config: &SegmentConfig,
    buffer_len: usize,
    trailing_silence_samples: usize,
    latest_rms: f32,
) -> SegmentDecision {
    let speech_end = buffer_len.saturating_sub(trailing_silence_samples);
    if speech_end >= config.min_segment_samples
        && trailing_silence_samples >= config.silence_samples
    {
        return SegmentDecision::Cut {
            speech_end,
            reason: CutReason::Silence,
        };
    }

    if buffer_len >= config.target_samples && latest_rms < config.vad_threshold {
        return SegmentDecision::Cut {
            speech_end: buffer_len,
            reason: CutReason::SoftLimitPause,
        };
    }

    if buffer_len >= config.target_samples + config.grace_samples {
        return SegmentDecision::Cut {
            speech_end: buffer_len,
            reason: CutReason::HardLimit,
        };
    }

    SegmentDecision::Continue
}

pub fn carry_after_cut(
    samples: &[f32],
    speech_end: usize,
    config: &SegmentConfig,
    reason: CutReason,
) -> Vec<f32> {
    match reason {
        CutReason::Silence => Vec::new(),
        CutReason::SoftLimitPause | CutReason::HardLimit => {
            let bounded_end = speech_end.min(samples.len());
            let start = bounded_end.saturating_sub(config.overlap_samples);
            samples[start..].to_vec()
        }
    }
}

pub fn dedupe_boundary(previous: &str, current: &str) -> String {
    let previous_words = previous.split_whitespace().collect::<Vec<_>>();
    let current_words = current.split_whitespace().collect::<Vec<_>>();
    let max_overlap = previous_words.len().min(current_words.len());

    for overlap in (1..=max_overlap).rev() {
        let previous_suffix = &previous_words[previous_words.len() - overlap..];
        let current_prefix = &current_words[..overlap];
        if previous_suffix
            .iter()
            .zip(current_prefix.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
        {
            return current_words[overlap..].join(" ");
        }
    }

    current.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SegmentConfig {
        SegmentConfig {
            sample_rate: 1_000,
            vad_threshold: 0.1,
            silence_samples: 300,
            target_samples: 10_000,
            grace_samples: 2_000,
            overlap_samples: 400,
            min_segment_samples: 1_200,
        }
    }

    #[test]
    fn cuts_on_natural_silence_after_minimum_duration() {
        let decision = decide_segment(&config(), 2_000, 300, 0.0);

        assert_eq!(
            decision,
            SegmentDecision::Cut {
                speech_end: 1_700,
                reason: CutReason::Silence,
            }
        );
    }

    #[test]
    fn does_not_cut_on_silence_before_minimum_duration() {
        let decision = decide_segment(&config(), 1_000, 300, 0.0);

        assert_eq!(decision, SegmentDecision::Continue);
    }

    #[test]
    fn after_target_cuts_on_first_low_energy_chunk() {
        let decision = decide_segment(&config(), 10_160, 0, 0.05);

        assert_eq!(
            decision,
            SegmentDecision::Cut {
                speech_end: 10_160,
                reason: CutReason::SoftLimitPause,
            }
        );
    }

    #[test]
    fn hard_limit_cuts_without_waiting_for_pause_after_grace_window() {
        let decision = decide_segment(&config(), 12_000, 0, 0.2);

        assert_eq!(
            decision,
            SegmentDecision::Cut {
                speech_end: 12_000,
                reason: CutReason::HardLimit,
            }
        );
    }

    #[test]
    fn carries_overlap_for_timed_cuts() {
        let samples = (0..1_200).map(|index| index as f32).collect::<Vec<_>>();

        let carried = carry_after_cut(&samples, 1_000, &config(), CutReason::HardLimit);

        assert_eq!(carried.len(), 600);
        assert_eq!(carried.first().copied(), Some(600.0));
        assert_eq!(carried.last().copied(), Some(1199.0));
    }

    #[test]
    fn clamps_carry_cut_points_to_sample_bounds() {
        let samples = (0..10).map(|index| index as f32).collect::<Vec<_>>();

        let carried = carry_after_cut(&samples, 20, &config(), CutReason::HardLimit);

        assert_eq!(carried, samples);
    }

    #[test]
    fn removes_duplicate_words_at_overlap_boundary() {
        assert_eq!(dedupe_boundary("hello world", "world again"), "again");
        assert_eq!(dedupe_boundary("this is a test", "is a test now"), "now");
        assert_eq!(
            dedupe_boundary("first segment", "second segment"),
            "second segment"
        );
    }
}
