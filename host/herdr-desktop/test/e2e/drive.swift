// Posts real key and mouse events to ONE process (the private Herdr window under test), by pid.
//
//   drive bounds <pid>                      the window's frame: x y w h (points, global)
//   drive activate <pid>                    make it the active app (needed for events to reach its window)
//   drive key <pid> <name> [shift|alt|ctrl|cmd ...]   enter, escape, tab, a..z, 0..9
//   drive type <pid> <text>                 each character as a key press
//   drive click <pid> <x> <y>               a left click at global point (x, y)
//   drive drag <pid> <x> <y> <x2> <y2>      press at (x, y), move, release at (x2, y2)
//
// Every event is posted only while that process is the frontmost app (checked each time). Needs Accessibility for the caller (the terminal).
import AppKit
import ApplicationServices

let args = CommandLine.arguments
guard args.count >= 3, let pid = pid_t(args[2]) else {
  FileHandle.standardError.write("usage: drive bounds|key|type|click <pid> …\n".data(using: .utf8)!)
  exit(2)
}

let keys: [String: CGKeyCode] = [
  "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14,
  "r": 15, "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29,
  "o": 31, "u": 32, "i": 34, "p": 35, "enter": 36, "l": 37, "j": 38, "k": 40, "n": 45, "m": 46, "tab": 48, "space": 49,
  "escape": 53, "delete": 51,
]

// Events go through the session's event tap, which delivers them to the frontmost app: posting to a
// pid directly did not reach the window. So the target must be frontmost at every event, or nothing
// is posted (the person's own apps never get a key).
func post(_ e: CGEvent) {
  if NSWorkspace.shared.frontmostApplication?.processIdentifier != pid {
    NSRunningApplication(processIdentifier: pid)?.activate(options: [])
    for _ in 0..<30 where NSWorkspace.shared.frontmostApplication?.processIdentifier != pid { usleep(50_000) }
  }
  guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else {
    FileHandle.standardError.write("refusing: pid \(pid) is not the frontmost app\n".data(using: .utf8)!)
    exit(3)
  }
  e.post(tap: .cghidEventTap)
}

func flags(_ names: [String]) -> CGEventFlags {
  var f: CGEventFlags = []
  for n in names {
    switch n {
    case "shift": f.insert(.maskShift)
    case "alt": f.insert(.maskAlternate)
    case "ctrl": f.insert(.maskControl)
    case "cmd": f.insert(.maskCommand)
    default: break
    }
  }
  return f
}

func press(_ code: CGKeyCode, _ f: CGEventFlags) {
  let src = CGEventSource(stateID: .hidSystemState)
  for down in [true, false] {
    let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: down)!
    e.flags = f
    post(e)
    usleep(15_000)
  }
}

func mouse(_ type: CGEventType, _ x: Double, _ y: Double) {
  let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left)!
  post(e)
}

switch args[1] {
case "bounds":
  let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .optionIncludingWindow], kCGNullWindowID) as? [[String: Any]] ?? []
  let all = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
  let mine = (list + all).filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid && ($0[kCGWindowLayer as String] as? Int) == 0 }
  guard let w = mine.max(by: { a, b in
    let ra = (a[kCGWindowBounds as String] as? [String: Double]) ?? [:], rb = (b[kCGWindowBounds as String] as? [String: Double]) ?? [:]
    return (ra["Width"] ?? 0) * (ra["Height"] ?? 0) < (rb["Width"] ?? 0) * (rb["Height"] ?? 0)
  }), let r = w[kCGWindowBounds as String] as? [String: Double] else { print("none"); exit(1) }
  print(Int(r["X"] ?? 0), Int(r["Y"] ?? 0), Int(r["Width"] ?? 0), Int(r["Height"] ?? 0))
case "activate":
  // Makes the process the active app so its window is the key window and takes events.
  if let app = NSRunningApplication(processIdentifier: pid) { print(app.activate(options: [.activateIgnoringOtherApps])) } else { print("no app"); exit(1) }
case "key":
  guard args.count >= 4, let code = keys[args[3].lowercased()] else { print("unknown key"); exit(2) }
  press(code, flags(Array(args.dropFirst(4))))
case "type":
  let src = CGEventSource(stateID: .hidSystemState)
  for ch in args[3] {
    let s = String(ch)
    for down in [true, false] {
      let e = CGEvent(keyboardEventSource: src, virtualKey: 0, keyDown: down)!
      var u = Array(s.utf16)
      e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: &u)
      post(e)
      usleep(12_000)
    }
  }
case "click":
  let x = Double(args[3])!, y = Double(args[4])!
  mouse(.mouseMoved, x, y); usleep(60_000)
  mouse(.leftMouseDown, x, y); usleep(60_000)
  mouse(.leftMouseUp, x, y)
case "drag":
  let x = Double(args[3])!, y = Double(args[4])!, x2 = Double(args[5])!, y2 = Double(args[6])!
  mouse(.leftMouseDown, x, y); usleep(60_000)
  mouse(.leftMouseDragged, (x + x2) / 2, (y + y2) / 2); usleep(60_000)
  mouse(.leftMouseUp, x2, y2)
default:
  print("unknown command"); exit(2)
}
