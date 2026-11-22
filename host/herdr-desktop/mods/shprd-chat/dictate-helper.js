// shprd's own on-device dictation helper: a small Swift program the composer builds once with the
// Mac's `swiftc` and runs while the mic is on. It records the default microphone, lets Apple's
// speech recognizer write the words down ON THIS MAC (`requiresOnDeviceRecognition`: no audio leaves
// the machine, no account and no key), and keeps no audio at all.
//
//   shprd-dictate <out> <level file>
//     level file: one number 0..1, ten times a second; or "!" and the words when the mic cannot work.
//     SIGINT : write <out>.txt as {"text": ...} (or {"error": ...}) and exit.   SIGTERM: throw it away.
//
// It lives in a string because a mod cannot read its own folder; `dictate.js` writes it out and builds it.

export const INFO_PLIST = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>app.shprd.dictate</string>
<key>CFBundleName</key><string>shprd dictation</string>
<key>NSMicrophoneUsageDescription</key><string>shprd listens while you dictate a message into the chat box. The audio is turned into words on this Mac and is never kept.</string>
<key>NSSpeechRecognitionUsageDescription</key><string>shprd turns your dictation into words on this Mac. Nothing is sent anywhere.</string>
</dict></plist>
`;

export const SOURCE = String.raw`import AVFoundation
import Darwin
import Foundation
import Speech

let args = CommandLine.arguments
if args.count < 3 { fputs("usage: shprd-dictate <out> <level file>\n", stderr); exit(2) }
let outPath = args[1] + ".txt"
let levelPath = args[2]

func say(_ text: String, to path: String) { try? text.write(toFile: path, atomically: true, encoding: .utf8) }
func die(_ why: String) -> Never { say("!" + why, to: levelPath); exit(1) }
func json(_ key: String, _ value: String) -> String {
  let data = (try? JSONSerialization.data(withJSONObject: [key: value])) ?? Data("{}".utf8)
  return String(data: data, encoding: .utf8) ?? "{}"
}

// macOS asks about the app "responsible" for a process (the Herdr window), which may have no words for a
// microphone prompt. This runs itself again disclaiming that, so the prompts are shprd dictation's own.
var child: pid_t = 0
func ownResponsibility() {
  if getenv("SHPRD_DICTATE_OWN") != nil { return }
  typealias Disclaim = @convention(c) (UnsafeMutablePointer<posix_spawnattr_t?>, Int32) -> Int32
  typealias Environ = @convention(c) () -> UnsafeMutablePointer<UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?>
  guard let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "responsibility_spawnattrs_setdisclaim"),
        let envSym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "_NSGetEnviron") else { return }
  let disclaim = unsafeBitCast(sym, to: Disclaim.self)
  let environ = unsafeBitCast(envSym, to: Environ.self)
  var attr: posix_spawnattr_t? = nil
  guard posix_spawnattr_init(&attr) == 0 else { return }
  defer { posix_spawnattr_destroy(&attr) }
  guard disclaim(&attr, 1) == 0 else { return }
  setenv("SHPRD_DICTATE_OWN", "1", 1)
  let path = Bundle.main.executablePath ?? args[0]
  let argv: [UnsafeMutablePointer<CChar>?] = args.map { strdup($0) } + [nil]
  var pid: pid_t = 0
  let rc = argv.withUnsafeBufferPointer { posix_spawn(&pid, path, nil, &attr, $0.baseAddress, environ().pointee) }
  if rc != 0 { unsetenv("SHPRD_DICTATE_OWN"); return }
  child = pid
  for s in [SIGTERM, SIGINT, SIGHUP] { signal(s) { sig in _ = kill(child, sig) } }
  var status: Int32 = 0
  while waitpid(pid, &status, 0) < 0 && errno == EINTR {}
  let sig = status & 0x7f
  exit(sig == 0 ? (status >> 8) & 0xff : 128 + sig)
}
ownResponsibility()

// Permissions: speech recognition, then the microphone.
let gate = DispatchSemaphore(value: 0)
var speechOK = false
SFSpeechRecognizer.requestAuthorization { status in speechOK = status == .authorized; gate.signal() }
gate.wait()
if !speechOK { die("Speech recognition is off for shprd dictation. Allow it in System Settings \u{203A} Privacy & Security \u{203A} Speech Recognition.") }
var micOK = false
AVCaptureDevice.requestAccess(for: .audio) { granted in micOK = granted; gate.signal() }
gate.wait()
if !micOK { die("Microphone access is off for shprd dictation. Allow it in System Settings \u{203A} Privacy & Security \u{203A} Microphone.") }

guard let recognizer = SFSpeechRecognizer(locale: Locale.current) ?? SFSpeechRecognizer(), recognizer.isAvailable else {
  die("Speech recognition isn't available on this Mac right now.")
}
guard recognizer.supportsOnDeviceRecognition else {
  die("This Mac can't write dictation down on-device for your language. Turn on Dictation in System Settings \u{203A} Keyboard to download it.")
}
let request = SFSpeechAudioBufferRecognitionRequest()
request.requiresOnDeviceRecognition = true
request.shouldReportPartialResults = true

var latest = ""
var failure: NSError? = nil
var ending = false
var finished = false
let lock = NSLock()

func finish() {
  lock.lock()
  if finished { lock.unlock(); return }
  finished = true
  let words = latest
  let err = failure
  lock.unlock()
  // "no speech detected" is not a failure: nothing was said.
  if words.isEmpty, let err = err, err.code != 1110, err.code != 203 {
    say(json("error", "Couldn't write that down on this Mac. Try again."), to: outPath)
  } else {
    say(json("text", words), to: outPath)
  }
  say("0", to: levelPath)
  exit(0)
}

let task = recognizer.recognitionTask(with: request) { result, error in
  lock.lock()
  if let result = result { latest = result.bestTranscription.formattedString }
  if let error = error { failure = error as NSError }
  let done = (result?.isFinal ?? false) || error != nil
  let stop = ending
  lock.unlock()
  if done && stop { finish() }
}

let engine = AVAudioEngine()
let input = engine.inputNode
let format = input.outputFormat(forBus: 0)
if format.sampleRate == 0 { die("No microphone was found.") }
var wrote = Date.distantPast
var peak: Float = 0
input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
  request.append(buffer)
  if let data = buffer.floatChannelData?[0] {
    var sum: Float = 0
    let n = Int(buffer.frameLength)
    for i in 0..<n { sum += data[i] * data[i] }
    peak = max(peak, min(1, (sum / Float(max(1, n))).squareRoot() * 8))
  }
  if Date().timeIntervalSince(wrote) >= 0.1 {
    say(String(format: "%.3f", peak), to: levelPath)
    wrote = Date()
    peak = 0
  }
}
do { try engine.start() } catch { die("The microphone didn't start.") }

signal(SIGINT, SIG_IGN)
signal(SIGTERM, SIG_IGN)
let stopSource = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
stopSource.setEventHandler {
  engine.stop()
  input.removeTap(onBus: 0)
  lock.lock(); ending = true; lock.unlock()
  request.endAudio()
  DispatchQueue.main.asyncAfter(deadline: .now() + 6) { finish() }
}
stopSource.resume()
let dropSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
dropSource.setEventHandler { engine.stop(); task.cancel(); exit(0) }
dropSource.resume()
// the program that started it gone: nobody will ask for the words
let parent = getppid()
let watch = DispatchSource.makeTimerSource(queue: .global())
watch.schedule(deadline: .now() + 0.5, repeating: 0.5)
watch.setEventHandler { if getppid() != parent { exit(0) } }
watch.resume()
dispatchMain()
`;
