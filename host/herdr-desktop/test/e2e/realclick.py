"""Real mouse clicks on a private Herdr window (SHPR-83).

The window has no pointer driver, so a click is made the way a person makes it: a CGEvent mouse
move + down + up at the place where the control is drawn. Where it is drawn is read from
`herdr-app shot` with macOS Vision's OCR (the word on the control), so nothing here knows a mod's
layout. Safety: `click` refuses unless the topmost window under the point is the private window's
(by its pid), so a covered or moved window never sends a click to the person's own apps.

Helpers are small Swift programs compiled once into a tools folder (`tools(dir)`).
"""
import os
import re
import subprocess

SWIFT_CLICK = r'''
import Cocoa
let a = CommandLine.arguments
guard a.count >= 4, let pid = Int32(a[1]), let rx = Double(a[2]), let ry = Double(a[3]) else { print("usage"); exit(2) }
let l = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String:Any]]
var best: (Double, Double, Double, Double)? = nil
for w in l where (w[kCGWindowOwnerPID as String] as! Int32) == pid && (w[kCGWindowLayer as String] as! Int) == 0 {
  let b = w[kCGWindowBounds as String] as! [String:Any]
  let r = (b["X"] as! Double, b["Y"] as! Double, b["Width"] as! Double, b["Height"] as! Double)
  if best == nil || r.2*r.3 > best!.2*best!.3 { best = r }
}
guard let win = best else { print("no window for pid"); exit(3) }
if a.count >= 5 && a[4] == "--bounds" { print("\(win.0) \(win.1) \(win.2) \(win.3)"); exit(0) }
let x = win.0 + rx, y = win.1 + ry
for w in l {
  let b = w[kCGWindowBounds as String] as! [String:Any]
  let bx = b["X"] as! Double, by = b["Y"] as! Double, bw = b["Width"] as! Double, bh = b["Height"] as! Double
  if x >= bx && x < bx+bw && y >= by && y < by+bh && (w[kCGWindowLayer as String] as! Int) == 0 {
    if (w[kCGWindowOwnerPID as String] as! Int32) != pid { print("refuse: topmost at point is pid \(w[kCGWindowOwnerPID as String]!)"); exit(4) }
    break
  }
}
let p = CGPoint(x: x, y: y)
let toPid = a.count >= 5 && a[4] == "--topid"
let saved = CGEvent(source: nil)?.location ?? p
func post(_ t: CGEventType) { let e = CGEvent(mouseEventSource: nil, mouseType: t, mouseCursorPosition: p, mouseButton: .left)!; if toPid { e.postToPid(pid) } else { e.post(tap: .cghidEventTap) }; usleep(80000) }
post(.mouseMoved); post(.leftMouseDown); post(.leftMouseUp)
if !toPid { CGWarpMouseCursorPosition(saved) }
print("clicked \(x),\(y)")
'''

SWIFT_OCR = r'''
import Vision
import Cocoa
let a = CommandLine.arguments
let img = NSImage(contentsOfFile: a[1])!
var r = CGRect(x:0,y:0,width:img.size.width,height:img.size.height)
let cg = img.cgImage(forProposedRect: &r, context: nil, hints: nil)!
let W = Double(cg.width), H = Double(cg.height)
let req = VNRecognizeTextRequest()
req.recognitionLevel = .accurate
req.usesLanguageCorrection = false
try VNImageRequestHandler(cgImage: cg).perform([req])
print("size \(Int(W)) \(Int(H))")
for o in req.results ?? [] {
  guard let c = o.topCandidates(1).first else { continue }
  let s = c.string
  var idx = s.startIndex
  for word in s.split(separator: " ", omittingEmptySubsequences: true) {
    let rg = s.range(of: String(word), range: idx..<s.endIndex)!
    idx = rg.upperBound
    if let wb = try? c.boundingBox(for: rg) {
      let bb = wb.boundingBox
      print("\(Int(bb.minX*W)) \(Int((1-bb.maxY)*H)) \(Int(bb.width*W)) \(Int(bb.height*H)) \(word)")
    }
  }
}
'''

_tools = {}


def tools(folder):
    """Compiles the two helpers once into `folder`."""
    if _tools:
        return _tools
    os.makedirs(folder, exist_ok=True)
    for name, src in (("click", SWIFT_CLICK), ("ocr", SWIFT_OCR)):
        path = f"{folder}/{name}"
        if not os.path.exists(path):
            open(f"{path}.swift", "w").write(src)
            subprocess.run(["swiftc", "-O", "-o", path, f"{path}.swift"], capture_output=True, check=True)
        _tools[name] = path
    return _tools


def window_pid(root_pid):
    """The pid that owns the private window: the launched process or one of its descendants."""
    out = subprocess.run(["ps", "-axo", "pid,ppid"], capture_output=True, text=True).stdout
    kids = {}
    for ln in out.splitlines()[1:]:
        p, pp = ln.split()
        kids.setdefault(int(pp), []).append(int(p))
    seen, todo = [], [root_pid]
    while todo:
        p = todo.pop()
        seen.append(p)
        todo += kids.get(p, [])
    return seen


def words(shot_path):
    """[(x, y, w, h, text)] in the shot's pixels, and the shot's size."""
    out = subprocess.run([_tools["ocr"], shot_path], capture_output=True, text=True).stdout.splitlines()
    size = tuple(int(v) for v in out[0].split()[1:3])
    got = []
    for ln in out[1:]:
        m = re.match(r"(\d+) (\d+) (\d+) (\d+) (.*)", ln)
        if m:
            got.append((int(m[1]), int(m[2]), int(m[3]), int(m[4]), m[5]))
    return got, size


def find(shot_path, text, row_of=None, nth=0, region=None):
    """The centre (px) of the `nth` word `text`. `row_of`: only on the same text line as that other word
    (a task id, say). `region`: (x0, y0, x1, y1) in px to look in."""
    ws, _ = words(shot_path)
    hits = [w for w in ws if w[4].strip(".,:").lower() == text.lower()]
    if region:
        hits = [w for w in hits if region[0] <= w[0] <= region[2] and region[1] <= w[1] <= region[3]]
    if row_of:
        rows = [w for w in ws if w[4].strip(".,:") == row_of]
        hits = [w for w in hits if any(abs((w[1] + w[3] / 2) - (r[1] + r[3] / 2)) < 40 for r in rows)]
    hits.sort(key=lambda w: (w[1], w[0]))
    if len(hits) <= nth:
        return None
    w = hits[nth]
    return (w[0] + w[2] / 2, w[1] + w[3] / 2)


def click_px(pids, px, size, log=print, mode="--topid"):
    """Clicks at a shot's pixel (px) in the window owned by one of `pids`."""
    last = ""
    for pid in pids:
        b = subprocess.run([_tools["click"], str(pid), "0", "0", "--bounds"], capture_output=True, text=True).stdout.split()
        if len(b) != 4:
            continue
        ww, wh = float(b[2]), float(b[3])
        sx, sy = ww / size[0], wh / size[1]
        r = subprocess.run([_tools["click"], str(pid), f"{px[0] * sx:.1f}", f"{px[1] * sy:.1f}", mode], capture_output=True, text=True)
        last = (r.stdout + r.stderr).strip()
        log(f"  click pid {pid} px {px[0]:.0f},{px[1]:.0f} -> {last}")
        if r.returncode == 0:
            return True
        if "no window" in last:
            continue
        return False
    return False
