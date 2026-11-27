#define WIN32_LEAN_AND_MEAN
#define _WIN32_WINNT 0x0601
#include <winsock2.h>
#include <windows.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MODEL_ID "vosk-model-small-cn-0.22"
#define MAX_HEADER 16384
#define MAX_WAV (256u * 1024u * 1024u)
#define MAX_BODY (MAX_WAV + 1024u * 1024u)
#define MAX_TEXT (256u * 1024u)

static void *(*model_new)(const char *);
static void (*model_free)(void *);
static void *(*recognizer_new)(void *, float);
static void (*recognizer_free)(void *);
static int (*recognizer_accept)(void *, const short *, int);
static const char *(*recognizer_result)(void *);
static const char *(*recognizer_final)(void *);

static int load_vosk(void) {
    HMODULE dll = LoadLibraryW(L"libvosk.dll");
    if (!dll) return 0;
#define LOAD(variable, name) do { \
    _Static_assert(sizeof(variable) == sizeof(FARPROC), "function pointer size"); \
    FARPROC function = GetProcAddress(dll, name); \
    if (!function) return 0; \
    memcpy(&variable, &function, sizeof(variable)); \
} while (0)
    LOAD(model_new, "vosk_model_new");
    LOAD(model_free, "vosk_model_free");
    LOAD(recognizer_new, "vosk_recognizer_new");
    LOAD(recognizer_free, "vosk_recognizer_free");
    LOAD(recognizer_accept, "vosk_recognizer_accept_waveform_s");
    LOAD(recognizer_result, "vosk_recognizer_result");
    LOAD(recognizer_final, "vosk_recognizer_final_result");
    void (*set_log_level)(int);
    LOAD(set_log_level, "vosk_set_log_level");
    set_log_level(-1);
#undef LOAD
    return 1;
}

static uint16_t little16(const unsigned char *data) {
    return (uint16_t)(data[0] | (data[1] << 8));
}

static uint32_t little32(const unsigned char *data) {
    return little16(data) | ((uint32_t)little16(data + 2) << 16);
}

static const unsigned char *find_bytes(const unsigned char *data, size_t length,
                                      const void *needle, size_t count) {
    while (length >= count) {
        const unsigned char *match = memchr(data, *(const unsigned char *)needle, length - count + 1);
        if (!match) return NULL;
        if (!memcmp(match, needle, count)) return match;
        length -= (size_t)(match + 1 - data);
        data = match + 1;
    }
    return NULL;
}

static int append_text(char *text, size_t *used, const char *json) {
    const char *start = json ? strstr(json, "\"text\"") : NULL;
    if (!start || !(start = strchr(start + 6, ':'))) return 0;
    while (*++start == ' ' || *start == '\t') {}
    if (*start++ != '"') return 0;
    const char *end = start;
    while (*end && *end != '"') {
        if (*end++ == '\\' && *end) ++end;
    }
    if (*end != '"') return 0;
    size_t count = (size_t)(end - start);
    if (!count) return 1;
    if (count >= MAX_TEXT - *used - 2) return 0;
    if (*used) text[(*used)++] = ' ';
    memcpy(text + *used, start, count);
    *used += count;
    text[*used] = 0;
    return 1;
}

static int transcribe(void *model, const unsigned char *wav, size_t length, char *text) {
    if (length < 12 || memcmp(wav, "RIFF", 4) || memcmp(wav + 8, "WAVE", 4)) return 0;
    const unsigned char *format = NULL, *samples = NULL;
    size_t sample_bytes = 0;
    size_t position = 12;
    while (length - position >= 8) {
        const unsigned char *chunk = wav + position;
        uint32_t count = little32(chunk + 4);
        if (count > length - position - 8) return 0;
        if (!memcmp(chunk, "fmt ", 4) && count >= 16) format = chunk + 8;
        if (!memcmp(chunk, "data", 4)) { samples = chunk + 8; sample_bytes = count; }
        position += 8 + count;
        if ((count & 1) && position < length) ++position;
    }
    if (!format || !samples || !sample_bytes || little16(format) != 1 || little16(format + 14) != 16) return 0;
    unsigned channels = little16(format + 2);
    unsigned rate = little32(format + 4);
    if ((channels != 1 && channels != 2) || rate < 8000 || rate > 48000 || sample_bytes % (channels * 2)) return 0;
    void *recognizer = recognizer_new(model, (float)rate);
    if (!recognizer) return -1;
    short pcm[4096];
    size_t used = 0;
    int result = 1;
    for (size_t offset = 0; offset < sample_bytes;) {
        int frames = 0;
        while (frames < 4096 && offset < sample_bytes) {
            int value = (int16_t)little16(samples + offset);
            if (channels == 2) value = (value + (int16_t)little16(samples + offset + 2)) / 2;
            pcm[frames++] = (short)value;
            offset += channels * 2;
        }
        int ready = recognizer_accept(recognizer, pcm, frames);
        if (ready < 0 || (ready && !append_text(text, &used, recognizer_result(recognizer)))) {
            result = -1;
            break;
        }
    }
    if (result > 0 && !append_text(text, &used, recognizer_final(recognizer))) result = -1;
    recognizer_free(recognizer);
    return result;
}

static int send_all(SOCKET client, const char *data, size_t length) {
    while (length) {
        int count = send(client, data, (int)length, 0);
        if (count <= 0) return 0;
        data += count;
        length -= (size_t)count;
    }
    return 1;
}

static void respond(SOCKET client, const char *status, const char *body) {
    size_t length = strlen(body);
    char headers[256];
    int count = snprintf(headers, sizeof(headers), "HTTP/1.1 %s\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: %zu\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n", status, length);
    if (count > 0 && send_all(client, headers, (size_t)count)) (void)send_all(client, body, length);
}

static const char *header_value(char *headers, const char *name) {
    const char *line = headers;
    size_t length = strlen(name);
    while (*line) {
        if (!_strnicmp(line, name, length) && line[length] == ':') {
            line += length + 1;
            while (*line == ' ' || *line == '\t') ++line;
            return line;
        }
        line += strlen(line) + 2;
    }
    return NULL;
}

static const unsigned char *multipart_audio(const unsigned char *body, size_t length, size_t *audio_bytes) {
    const unsigned char *end = body + length;
    const unsigned char *line = find_bytes(body, length, "\r\n", 2);
    if (!line || line - body < 3 || line - body > 72 || memcmp(body, "--", 2)) return NULL;
    char delimiter[75] = "\r\n";
    size_t delimiter_size = 2 + (size_t)(line - body);
    memcpy(delimiter + 2, body, delimiter_size - 2);
    const unsigned char *part = line + 2;
    while (part < end) {
        const unsigned char *content = find_bytes(part, (size_t)(end - part), "\r\n\r\n", 4);
        if (!content) return NULL;
        int is_file = find_bytes(part, (size_t)(content - part), " name=\"file\"", 12) != NULL;
        content += 4;
        const unsigned char *next = content;
        for (;;) {
            next = find_bytes(next, (size_t)(end - next), delimiter, delimiter_size);
            if (!next || (size_t)(end - next) < delimiter_size + 2) return NULL;
            const unsigned char *ending = next + delimiter_size;
            if (!memcmp(ending, "\r\n", 2) || !memcmp(ending, "--", 2)) break;
            ++next;
        }
        if (is_file) {
            *audio_bytes = (size_t)(next - content);
            return content;
        }
        if (!memcmp(next + delimiter_size, "--", 2)) return NULL;
        part = next + delimiter_size + 2;
    }
    return NULL;
}

static void serve(SOCKET client, void *model, const char *authority) {
    char buffer[MAX_HEADER + 1];
    size_t received = 0;
    char *separator;
    for (;;) {
        int count = recv(client, buffer + received, (int)(MAX_HEADER - received), 0);
        if (count <= 0) return;
        received += (size_t)count;
        buffer[received] = 0;
        separator = strstr(buffer, "\r\n\r\n");
        if (separator) break;
        if (received == MAX_HEADER) return;
    }
    char *body_start = separator + 4;
    char *line_end = strstr(buffer, "\r\n");
    if (line_end == separator) return;
    *line_end = 0;
    char *headers = line_end + 2;
    for (char *line = headers; line < separator;) {
        char *end = strstr(line, "\r\n");
        *end = 0;
        line = end + 2;
    }
    separator[2] = 0;
    const char *host = header_value(headers, "Host");
    if (!host || strcmp(host, authority) || header_value(headers, "Origin")) {
        respond(client, "403 Forbidden", "{\"error\":{\"message\":\"Local Speaky clients only\"}}");
        return;
    }
    if (!strncmp(buffer, "GET /health HTTP/", 17)) {
        respond(client, "200 OK", "{\"status\":\"ready\"}");
        return;
    }
    if (!strncmp(buffer, "GET /v1/models HTTP/", 20)) {
        respond(client, "200 OK", "{\"object\":\"list\",\"data\":[{\"id\":\"" MODEL_ID "\",\"object\":\"model\"}]}");
        return;
    }
    if (strncmp(buffer, "POST /v1/audio/transcriptions HTTP/", 35)) {
        respond(client, "404 Not Found", "{\"error\":{\"message\":\"Unknown path\"}}");
        return;
    }
    const char *count_field = header_value(headers, "Content-Length");
    const char *content_type = header_value(headers, "Content-Type");
    char *count_end;
    unsigned long length = count_field ? strtoul(count_field, &count_end, 10) : 0;
    if (!length || length > MAX_BODY || *count_end || !content_type || _strnicmp(content_type, "multipart/form-data;", 20)) {
        respond(client, "400 Bad Request", "{\"error\":{\"message\":\"Expected a multipart WAV upload within 256 MiB\"}}");
        return;
    }
    if (header_value(headers, "Expect") && !send_all(client, "HTTP/1.1 100 Continue\r\n\r\n", 25)) return;
    unsigned char *body = malloc(length);
    if (!body) { respond(client, "503 Service Unavailable", "{}"); return; }
    size_t total = received - (size_t)(body_start - buffer);
    if (total > length) total = length;
    memcpy(body, body_start, total);
    while (total < length) {
        int count = recv(client, (char *)body + total, (int)(length - total), 0);
        if (count <= 0) { free(body); return; }
        total += (size_t)count;
    }
    size_t audio_bytes;
    const unsigned char *audio = multipart_audio(body, length, &audio_bytes);
    char *reply = malloc(MAX_TEXT + 32);
    int result = -1;
    if (audio && audio_bytes > MAX_WAV) {
        respond(client, "413 Payload Too Large", "{\"error\":{\"message\":\"WAV exceeds 256 MiB\"}}");
        free(reply);
        free(body);
        return;
    }
    if (reply && audio) {
        strcpy(reply, "{\"text\":\"");
        result = transcribe(model, audio, audio_bytes, reply + strlen(reply));
    }
    if (!audio || result == 0) {
        respond(client, "422 Unprocessable Entity", "{\"error\":{\"message\":\"Expected mono or stereo PCM16 WAV at 8 to 48 kHz\"}}");
    } else if (result < 0) {
        respond(client, "500 Internal Server Error", "{\"error\":{\"message\":\"Vosk could not transcribe this audio\"}}");
    } else {
        strcat(reply, "\"}");
        respond(client, "200 OK", reply);
    }
    free(reply);
    free(body);
}

int main(int argc, char **argv) {
    char *port_end;
    long port = argc == 2 ? strtol(argv[1], &port_end, 10) : 0;
    if (port < 1 || port > 65535 || *port_end || !load_vosk()) return 1;
    void *model = model_new("model");
    if (!model) return 2;
    WSADATA winsock;
    if (WSAStartup(MAKEWORD(2, 2), &winsock)) { model_free(model); return 3; }
    SOCKET listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    BOOL exclusive = TRUE;
    struct sockaddr_in address = {0};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = htons((u_short)port);
    if (listener == INVALID_SOCKET || setsockopt(listener, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (char *)&exclusive, sizeof(exclusive)) ||
        bind(listener, (struct sockaddr *)&address, sizeof(address)) || listen(listener, SOMAXCONN)) {
        if (listener != INVALID_SOCKET) closesocket(listener);
        WSACleanup();
        model_free(model);
        return 4;
    }
    char authority[32];
    snprintf(authority, sizeof(authority), "127.0.0.1:%ld", port);
    for (;;) {
        SOCKET client = accept(listener, NULL, NULL);
        if (client == INVALID_SOCKET) break;
        DWORD timeout = 10000;
        setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, (char *)&timeout, sizeof(timeout));
        setsockopt(client, SOL_SOCKET, SO_SNDTIMEO, (char *)&timeout, sizeof(timeout));
        serve(client, model, authority);
        closesocket(client);
    }
    closesocket(listener);
    WSACleanup();
    model_free(model);
    return 0;
}
