// Pass whisper.cpp's versioned parameter structs by value from C, keeping their
// native layout out of Python. Build for x64 with the v1.8.3 headers and DLL:
// gcc -O2 -shared -s -I headers whisper_gpu_bridge.c whisper.dll -o whisper_gpu_bridge.dll
#include "whisper.h"

__declspec(dllexport) struct whisper_context *speaky_whisper_open(const char *model, int device) {
    struct whisper_context_params params = whisper_context_default_params();
    params.use_gpu = true;
    params.gpu_device = device;
    return whisper_init_from_file_with_params(model, params);
}

__declspec(dllexport) int speaky_whisper_transcribe(struct whisper_context *ctx,
        const float *audio, int count, const char *language, const char *prompt) {
    struct whisper_full_params params = whisper_full_default_params(WHISPER_SAMPLING_BEAM_SEARCH);
    params.language = language;
    params.initial_prompt = prompt;
    params.n_threads = 4;
    params.no_context = true;
    params.no_timestamps = true;
    params.print_progress = false;
    params.print_realtime = false;
    params.print_timestamps = false;
    return whisper_full(ctx, params, audio, count);
}
