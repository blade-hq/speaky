#!/usr/bin/env bash
set -euo pipefail

git clone --depth 1 --branch v1.9.1 https://github.com/ggml-org/whisper.cpp.git target/whisper-source
cmake -S target/whisper-source -B target/whisper-build \
  -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF \
  -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_EXAMPLES=ON -DWHISPER_BUILD_SERVER=ON \
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_POLICY_DEFAULT_CMP0091=NEW \
  -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded
cmake --build target/whisper-build --config Release --target whisper-server --parallel
mkdir -p target/whisper-runtime
if [[ "${RUNNER_OS:-}" == Windows ]]; then
  cp target/whisper-build/bin/Release/whisper-server.exe target/whisper-runtime/
else
  cp target/whisper-build/bin/whisper-server target/whisper-runtime/
fi
cp target/whisper-source/LICENSE target/whisper-runtime/LICENSE
