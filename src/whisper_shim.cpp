#include "whisper.h"

#include <stdbool.h>

extern "C" int sv_whisper_full_configured(
    whisper_context * ctx,
    const float * samples,
    int n_samples,
    const char * language,
    bool detect_language,
    int n_threads
) {
    whisper_full_params params =
        whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
    params.print_progress = false;
    params.print_realtime = false;
    params.print_timestamps = false;
    params.no_timestamps = true;
    params.single_segment = true;
    params.translate = false;
    params.n_threads = n_threads;
    params.language = language;
    params.detect_language = detect_language;

    return whisper_full(ctx, params, samples, n_samples);
}
