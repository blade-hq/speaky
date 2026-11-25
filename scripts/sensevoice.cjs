const path = require("node:path");
const [modulePath, directory, audio, language] = process.argv.slice(2);
const sherpa = require(modulePath);
const recognizer = new sherpa.OfflineRecognizer({
  featConfig: { sampleRate: 16000, featureDim: 80 },
  modelConfig: {
    senseVoice: {
      model: path.join(directory, "model.int8.onnx"),
      language,
      useInverseTextNormalization: 1,
    },
    tokens: path.join(directory, "tokens.txt"),
    numThreads: 4,
    provider: "cpu",
  },
});
const stream = recognizer.createStream();
stream.acceptWaveform(sherpa.readWave(audio));
recognizer.decode(stream);
process.stdout.write(recognizer.getResult(stream).text);
